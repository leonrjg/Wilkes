//! Bringing one site's cookies from the user's own browser into the browser
//! pane, so a sign-in made there carries over without signing in again.
//!
//! Always one site at a time, and only when the user asks: a browser's cookie
//! store holds every session the user has, and the pane needs only the one in
//! front of it. Reading it is `rookie`'s; deciding which cookies belong to the
//! site is done here, because rookie's own filter is a substring match (asking
//! for `x.com` also returns `box.com`'s) built into its SQL from the string it
//! is given -- so it is only ever given a validated host name.

use serde::{Deserialize, Serialize};
use tauri::webview::cookie::{time::OffsetDateTime, Cookie, SameSite};
use tauri::Window;

/// The browsers a site's cookies can be read from. Safari is left out: its
/// store needs Full Disk Access, which is not worth asking for this.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CookieSource {
    Chrome,
    Arc,
    Brave,
    Edge,
    Firefox,
    Zen,
    Chromium,
    Vivaldi,
    Opera,
}

impl CookieSource {
    const ALL: [CookieSource; 9] = [
        CookieSource::Chrome,
        CookieSource::Arc,
        CookieSource::Brave,
        CookieSource::Edge,
        CookieSource::Firefox,
        CookieSource::Zen,
        CookieSource::Chromium,
        CookieSource::Vivaldi,
        CookieSource::Opera,
    ];

    fn name(self) -> &'static str {
        match self {
            CookieSource::Chrome => "Chrome",
            CookieSource::Arc => "Arc",
            CookieSource::Brave => "Brave",
            CookieSource::Edge => "Edge",
            CookieSource::Firefox => "Firefox",
            CookieSource::Zen => "Zen",
            CookieSource::Chromium => "Chromium",
            CookieSource::Vivaldi => "Vivaldi",
            CookieSource::Opera => "Opera",
        }
    }

    fn read(self, site: &str) -> rookie::Result<Vec<rookie::enums::Cookie>> {
        let domains = Some(vec![site.to_string()]);
        match self {
            CookieSource::Chrome => rookie::chrome(domains),
            CookieSource::Arc => rookie::arc(domains),
            CookieSource::Brave => rookie::brave(domains),
            CookieSource::Edge => rookie::edge(domains),
            CookieSource::Firefox => rookie::firefox(domains),
            CookieSource::Zen => rookie::zen(domains),
            CookieSource::Chromium => rookie::chromium(domains),
            CookieSource::Vivaldi => rookie::vivaldi(domains),
            CookieSource::Opera => rookie::opera(domains),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CookieSourceInfo {
    id: CookieSource,
    name: &'static str,
}

/// A site as the user named it, as a bare host name: `www.` and a leading dot
/// dropped, and nothing that is not a host name let through.
fn site_domain(input: &str) -> Result<String, String> {
    let site = input.trim().to_ascii_lowercase();
    let site = site.trim_start_matches('.');
    let site = site.strip_prefix("www.").unwrap_or(site);
    let valid = site.contains('.')
        && !site.contains("..")
        && !site.ends_with('.')
        && site
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if valid {
        Ok(site.to_string())
    } else {
        Err(format!("\"{}\" is not a site's domain", input.trim()))
    }
}

/// Whether a cookie set for `cookie_domain` belongs to `site`: the site itself
/// or one of its subdomains.
fn belongs_to(cookie_domain: &str, site: &str) -> bool {
    let domain = cookie_domain.trim_start_matches('.').to_ascii_lowercase();
    domain == site || domain.ends_with(&format!(".{site}"))
}

fn same_site(value: i64) -> Option<SameSite> {
    match value {
        0 => Some(SameSite::None),
        1 => Some(SameSite::Lax),
        2 => Some(SameSite::Strict),
        _ => None,
    }
}

/// The domain to build a cookie with so that the pane's store gets
/// `cookie_domain` as the browser had it. A leading dot is what makes a cookie
/// the whole site's rather than its host's alone, and the webview reads the
/// domain back through `Cookie::domain`, which strips one -- so a site-wide
/// cookie is built with a second.
fn store_domain(cookie_domain: String) -> String {
    if cookie_domain.starts_with('.') {
        format!(".{cookie_domain}")
    } else {
        cookie_domain
    }
}

/// The cookie as the pane's store takes it, or nothing for one that has
/// already expired. `Secure` and `HttpOnly` are only ever set, never set to
/// false: the macOS webview writes a false flag as "FALSE", which WebKit
/// reads as set, so a cookie its page's scripts should see would be hidden
/// from them.
fn pane_cookie(cookie: rookie::enums::Cookie, now: i64) -> Option<Cookie<'static>> {
    let expires = match cookie.expires {
        Some(seconds) => {
            let seconds = i64::try_from(seconds).ok()?;
            if seconds <= now {
                return None;
            }
            Some(OffsetDateTime::from_unix_timestamp(seconds).ok()?)
        }
        None => None,
    };
    let mut builder = Cookie::build((cookie.name, cookie.value))
        .domain(store_domain(cookie.domain))
        .path(cookie.path);
    if cookie.secure {
        builder = builder.secure(true);
    }
    if cookie.http_only {
        builder = builder.http_only(true);
    }
    if let Some(expires) = expires {
        builder = builder.expires(expires);
    }
    if let Some(same_site) = same_site(cookie.same_site) {
        builder = builder.same_site(same_site);
    }
    Some(builder.build())
}

#[tauri::command]
pub fn browser_cookie_sources() -> Vec<CookieSourceInfo> {
    CookieSource::ALL
        .iter()
        .map(|&id| CookieSourceInfo {
            id,
            name: id.name(),
        })
        .collect()
}

/// Copy `site`'s cookies from `source` into the calling window's pane and
/// return how many there were. Reading a Chromium browser's store asks the
/// OS for its key, which on macOS shows a Keychain prompt.
#[tauri::command]
pub async fn browser_pane_import_cookies(
    window: Window,
    source: CookieSource,
    site: String,
) -> Result<usize, String> {
    let site = site_domain(&site)?;
    let webview = crate::browser_pane::pane(&window)?;
    let read = {
        let site = site.clone();
        tauri::async_runtime::spawn_blocking(move || source.read(&site))
    };
    let cookies = read
        .await
        .map_err(|e| format!("Reading {}'s cookies stopped: {e}", source.name()))?
        .map_err(|e| format!("Could not read {}'s cookies: {e}", source.name()))?;

    let now = OffsetDateTime::now_utc().unix_timestamp();
    let mut imported = 0;
    for cookie in cookies {
        if !belongs_to(&cookie.domain, &site) {
            continue;
        }
        let Some(cookie) = pane_cookie(cookie, now) else {
            continue;
        };
        webview.set_cookie(cookie).map_err(|e| e.to_string())?;
        imported += 1;
    }
    Ok(imported)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(domain: &str, expires: Option<u64>, same_site: i64) -> rookie::enums::Cookie {
        rookie::enums::Cookie {
            domain: domain.to_string(),
            path: "/".to_string(),
            secure: true,
            expires,
            name: "session".to_string(),
            value: "abc".to_string(),
            http_only: true,
            same_site,
        }
    }

    #[test]
    fn a_site_wide_cookie_stays_site_wide_and_a_host_cookie_stays_the_hosts() {
        let site_wide = pane_cookie(cookie(".example.org", None, -1), 1_000).unwrap();
        assert_eq!(site_wide.domain(), Some(".example.org"));
        let host = pane_cookie(cookie("id.example.org", None, -1), 1_000).unwrap();
        assert_eq!(host.domain(), Some("id.example.org"));
    }

    #[test]
    fn flags_a_cookie_does_not_have_are_left_unset() {
        let mut readable = cookie("example.org", None, -1);
        readable.secure = false;
        readable.http_only = false;
        let pane = pane_cookie(readable, 1_000).unwrap();
        assert_eq!(pane.secure(), None);
        assert_eq!(pane.http_only(), None);
    }

    #[test]
    fn a_site_is_a_bare_host_name() {
        assert_eq!(site_domain(" www.Example.org ").unwrap(), "example.org");
        assert_eq!(
            site_domain(".journals.example.org").unwrap(),
            "journals.example.org"
        );
        assert!(site_domain("example").is_err());
        assert!(site_domain("example..org").is_err());
        assert!(site_domain("x.com' OR 1=1 --").is_err());
        assert!(site_domain("%").is_err());
        assert!(site_domain("").is_err());
    }

    #[test]
    fn a_cookie_belongs_to_its_site_and_subdomains_only() {
        assert!(belongs_to("example.org", "example.org"));
        assert!(belongs_to(".example.org", "example.org"));
        assert!(belongs_to("id.example.org", "example.org"));
        assert!(!belongs_to("badexample.org", "example.org"));
        assert!(!belongs_to("example.org.evil.com", "example.org"));
        assert!(!belongs_to("org", "example.org"));
    }

    #[test]
    fn a_cookie_keeps_its_attributes_in_the_pane() {
        let pane = pane_cookie(cookie(".example.org", Some(2_000_000_000), 1), 1_000).unwrap();
        assert_eq!(pane.name(), "session");
        assert_eq!(pane.value(), "abc");
        assert_eq!(pane.path(), Some("/"));
        assert_eq!(pane.secure(), Some(true));
        assert_eq!(pane.http_only(), Some(true));
        assert_eq!(pane.same_site(), Some(SameSite::Lax));
        assert_eq!(
            pane.expires_datetime().map(|t| t.unix_timestamp()),
            Some(2_000_000_000)
        );
    }

    #[test]
    fn a_session_cookie_stays_one_and_an_expired_cookie_is_dropped() {
        let session = pane_cookie(cookie("example.org", None, -1), 1_000).unwrap();
        assert!(session.expires().is_none());
        assert_eq!(session.same_site(), None);
        assert!(pane_cookie(cookie("example.org", Some(999), 0), 1_000).is_none());
    }
}
