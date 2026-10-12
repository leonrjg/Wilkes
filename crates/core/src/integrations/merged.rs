//! One declaration for decoding, with routes back to each provider's controls.
use std::collections::{BTreeMap, BTreeSet};

use super::custom::manifest::{BrowseControls, FilterControl, SortControl};
use crate::generate::tasks::browse_request::BrowseComposition;

pub struct MergedControls {
    pub controls: BrowseControls,
    providers: BTreeMap<String, ProviderRoutes>,
}

struct ProviderRoutes {
    name: String,
    controls: BrowseControls,
    filters: BTreeMap<String, FilterControl>,
    orders: BTreeMap<String, String>,
}

fn shared_value<'a>(filter: &'a FilterControl, own: &'a str) -> &'a str {
    filter
        .equivalent_values
        .get(own)
        .map(String::as_str)
        .unwrap_or(own)
}

fn same_declaration(a: &FilterControl, b: &FilterControl) -> bool {
    a.equivalent_to.is_none()
        && b.equivalent_to.is_none()
        && a.name == b.name
        && a.label == b.label
        && a.group == b.group
        && a.input == b.input
        && a.options == b.options
        && a.modes == b.modes
}

/// Provider order determines labels and the order of controls and options.
pub fn merge(declared: &[(&str, &str, &BrowseControls)]) -> MergedControls {
    let mut merged = MergedControls {
        controls: BrowseControls {
            keyword: false,
            // Merged, a keyword is needed only when every provider needs one.
            keyword_required: !declared.is_empty()
                && declared.iter().all(|(_, _, c)| c.keyword_required),
            total: false,
            sort: None,
            modes: Vec::new(),
            filters: Vec::new(),
        },
        providers: BTreeMap::new(),
    };
    // Shared identities retain their names even when an earlier untagged
    // filter uses that name. Also reserve actual names against generated ones.
    let tagged: BTreeSet<_> = declared
        .iter()
        .flat_map(|(_, _, controls)| controls.filters.iter())
        .filter(|f| f.input != "result")
        .filter_map(|f| f.equivalent_to.clone())
        .collect();
    let mut reserved = tagged.clone();
    reserved.extend(["search".to_string(), "sort".to_string()]);
    reserved.extend(
        declared
            .iter()
            .flat_map(|(_, _, c)| c.filters.iter().map(|f| f.name.clone())),
    );
    let mut originals: Vec<FilterControl> = Vec::new();
    for (id, name, controls) in declared {
        let mut routes = ProviderRoutes {
            name: name.to_string(),
            controls: (*controls).clone(),
            filters: BTreeMap::new(),
            orders: BTreeMap::new(),
        };
        merged.controls.keyword |= controls.keyword;
        merged.controls.total |= controls.total;
        for mode in &controls.modes {
            if !merged.controls.modes.iter().any(|m| m.value == mode.value) {
                merged.controls.modes.push(mode.clone());
            }
        }
        for filter in controls.filters.iter().filter(|f| f.input != "result") {
            let existing =
                originals
                    .iter()
                    .position(|f| match (&filter.equivalent_to, &f.equivalent_to) {
                        (Some(a), Some(b)) => a == b,
                        (None, None) => same_declaration(filter, f),
                        _ => false,
                    });
            let index = if let Some(index) = existing {
                index
            } else {
                let mut composed = filter.clone();
                composed.name = filter
                    .equivalent_to
                    .clone()
                    .unwrap_or_else(|| filter.name.clone());
                if filter.equivalent_to.is_none()
                    && (tagged.contains(&composed.name)
                        || matches!(composed.name.as_str(), "search" | "sort")
                        || merged
                            .controls
                            .filters
                            .iter()
                            .any(|f| f.name == composed.name))
                {
                    let prefix = format!("{id}_{}", filter.name);
                    composed.name = prefix.clone();
                    let mut suffix = 2;
                    while reserved.contains(&composed.name) {
                        composed.name = format!("{prefix}_{suffix}");
                        suffix += 1;
                    }
                    reserved.insert(composed.name.clone());
                }
                for option in &mut composed.options {
                    option.value = shared_value(filter, &option.value).to_string();
                }
                // The merged declaration has shared values, not provider values.
                composed.equivalent_values.clear();
                originals.push(filter.clone());
                merged.controls.filters.push(composed);
                merged.controls.filters.len() - 1
            };
            let composed = &mut merged.controls.filters[index];
            for option in &filter.options {
                let value = shared_value(filter, &option.value);
                if !composed.options.iter().any(|o| o.value == value) {
                    let mut option = option.clone();
                    option.value = value.to_string();
                    composed.options.push(option);
                }
            }
            if composed.modes.is_empty() && !filter.modes.is_empty() {
                composed.modes = filter.modes.clone();
            }
            routes.filters.insert(composed.name.clone(), filter.clone());
        }
        if let Some(sort) = &controls.sort {
            let shared = |own: &String| sort.equivalent_to.get(own).unwrap_or(own).clone();
            let composed = merged.controls.sort.get_or_insert_with(|| SortControl {
                default: shared(&sort.default),
                keyword_default: sort.keyword_default.as_ref().map(shared),
                options: Vec::new(),
                equivalent_to: BTreeMap::new(),
            });
            for option in &sort.options {
                let value = shared(&option.value);
                if !composed.options.iter().any(|o| o.value == value) {
                    let mut option = option.clone();
                    option.value = value.clone();
                    composed.options.push(option);
                }
                // When several own orders share an identity, declaration order wins.
                routes
                    .orders
                    .entry(value)
                    .or_insert_with(|| option.value.clone());
            }
        }
        merged.providers.insert(id.to_string(), routes);
    }
    merged
}

impl MergedControls {
    /// Translate only declared parts. Unsupported parts remain visible as notes.
    pub fn project(&self, provider: &str, composed: &BrowseComposition) -> BrowseComposition {
        let routes = self
            .providers
            .get(provider)
            .expect("projection needs a selected provider");
        let mut projected = BrowseComposition {
            notes: composed.notes.clone(),
            ..BrowseComposition::default()
        };
        if let Some(keyword) = &composed.keyword {
            if routes.controls.keyword {
                projected.keyword = Some(keyword.clone());
            } else {
                projected
                    .notes
                    .push(format!("{} does not offer a keyword search", routes.name));
            }
        }
        for (name, values) in &composed.filters {
            let year_boundary = match name.as_str() {
                "year_from" => Some(("published_from", "01-01")),
                "year_to" => Some(("published_to", "12-31")),
                _ => None,
            };
            let date_route = year_boundary.and_then(|(date, boundary)| {
                let is_year = self
                    .controls
                    .filters
                    .iter()
                    .any(|f| f.name == *name && f.equivalent_to.as_deref() == Some(name));
                if !is_year
                    || routes
                        .filters
                        .values()
                        .any(|f| f.equivalent_to.as_deref() == Some(name))
                {
                    return None;
                }
                routes
                    .filters
                    .values()
                    .find(|f| f.equivalent_to.as_deref() == Some(date))
                    .map(|f| (f, boundary))
            });
            let (filter, boundary) = match routes.filters.get(name) {
                Some(filter) => (filter, None),
                None => match date_route {
                    Some((filter, boundary)) => (filter, Some(boundary)),
                    None => {
                        projected.notes.push(format!(
                            "{} has no filter for {name}, so it was left out",
                            routes.name
                        ));
                        continue;
                    }
                },
            };
            let mut own = Vec::new();
            for value in values {
                let value = if let Some(boundary) = boundary {
                    match value.parse::<u32>() {
                        Ok(year) if year <= 9999 => format!("{year:04}-{boundary}"),
                        _ => {
                            projected
                                .notes
                                .push(format!("{name} “{value}” cannot be represented as a date"));
                            continue;
                        }
                    }
                } else if filter.input == "options" {
                    match filter
                        .options
                        .iter()
                        .find(|o| shared_value(filter, &o.value) == value)
                    {
                        Some(option) => option.value.clone(),
                        None => {
                            projected.notes.push(format!(
                                "{} has no {} option for “{value}”, so it was left out",
                                routes.name, filter.label
                            ));
                            continue;
                        }
                    }
                } else {
                    value.clone()
                };
                if filter.modes.is_empty() && !own.is_empty() {
                    projected.notes.push(format!(
                        "{} takes only one {} value; “{value}” was left out",
                        routes.name, filter.label
                    ));
                } else {
                    own.push(value);
                }
            }
            if !own.is_empty() {
                if projected
                    .filters
                    .iter()
                    .any(|(name, _)| name == &filter.name)
                {
                    projected.notes.push(format!("{name} was left out because {} already has a value from another part of the request", filter.label));
                } else {
                    projected.filters.push((filter.name.clone(), own));
                }
            }
        }
        if let Some(order) = &composed.sort {
            if let Some(own) = routes.orders.get(order) {
                projected.sort = Some(own.clone());
            } else {
                projected.notes.push(format!(
                    "{} does not offer the order “{order}”, so it was left out",
                    routes.name
                ));
            }
        }
        projected
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::custom::manifest::{ControlOption, Manifest};

    fn option(value: &str) -> ControlOption {
        ControlOption {
            value: value.into(),
            label: value.into(),
        }
    }
    fn filter(name: &str, tag: Option<&str>, input: &str, values: &[&str]) -> FilterControl {
        FilterControl {
            name: name.into(),
            label: name.into(),
            group: None,
            input: input.into(),
            options: values.iter().map(|v| option(v)).collect(),
            modes: Vec::new(),
            from_result: None,
            equivalent_to: tag.map(str::to_string),
            equivalent_values: BTreeMap::new(),
        }
    }
    fn controls(filters: Vec<FilterControl>) -> BrowseControls {
        BrowseControls {
            keyword: false,
            keyword_required: false,
            total: false,
            sort: None,
            modes: Vec::new(),
            filters,
        }
    }

    #[test]
    fn shared_controls_project_own_values_dates_orders_and_notes() {
        let mut kind = filter(
            "kind",
            Some("type"),
            "options",
            &["dissertation", "article"],
        );
        kind.equivalent_values
            .insert("dissertation".into(), "thesis".into());
        kind.modes = vec!["any".into()];
        let mut a = controls(vec![
            kind,
            filter("since", Some("year_from"), "integer", &[]),
            filter("until", Some("year_to"), "integer", &[]),
            filter("local", None, "toggle", &[]),
        ]);
        a.keyword = true;
        a.sort = Some(SortControl {
            default: "citations".into(),
            keyword_default: None,
            options: vec![option("citations"), option("local_order")],
            equivalent_to: BTreeMap::from([("citations".into(), "most_cited".into())]),
        });
        let b = controls(vec![
            filter(
                "category",
                Some("type"),
                "options",
                &["article", "thesis", "review"],
            ),
            filter("after", Some("published_from"), "date", &[]),
            filter("before", Some("published_to"), "date", &[]),
        ]);
        let merged = merge(&[("a", "A", &a), ("b", "B", &b)]);
        let kind = &merged.controls.filters[0];
        assert_eq!(kind.name, "type");
        assert_eq!(kind.label, "kind");
        assert_eq!(
            kind.options
                .iter()
                .map(|o| o.value.as_str())
                .collect::<Vec<_>>(),
            ["thesis", "article", "review"]
        );
        assert!(!kind.modes.is_empty());
        let composition = BrowseComposition {
            keyword: Some("cells".into()),
            filters: vec![
                (
                    "type".into(),
                    vec!["review".into(), "thesis".into(), "article".into()],
                ),
                ("year_from".into(), vec!["2020".into()]),
                ("year_to".into(), vec!["2025".into()]),
                ("local".into(), vec!["true".into()]),
            ],
            sort: Some("most_cited".into()),
            notes: vec!["decode note".into()],
        };
        let own = merged.project("a", &composition);
        assert_eq!(own.keyword, composition.keyword);
        assert_eq!(
            own.filters[0],
            ("kind".into(), vec!["dissertation".into(), "article".into()])
        );
        assert_eq!(own.sort.as_deref(), Some("citations"));
        assert!(own.notes.iter().any(|n| n.contains("review")));
        let own = merged.project("b", &composition);
        assert_eq!(own.keyword, None);
        assert_eq!(own.sort, None);
        assert_eq!(
            own.filters,
            vec![
                ("category".into(), vec!["review".into()]),
                ("after".into(), vec!["2020-01-01".into()]),
                ("before".into(), vec!["2025-12-31".into()])
            ]
        );
        for expected in ["decode note", "keyword", "only one", "local", "order"] {
            assert!(
                own.notes.iter().any(|n| n.contains(expected)),
                "{expected}: {:?}",
                own.notes
            );
        }
        let reverse = merge(&[("b", "B", &b), ("a", "A", &a)]);
        assert!(!reverse.controls.filters[0].modes.is_empty());
    }

    #[test]
    fn untagged_declarations_deduplicate_and_conflicts_stay_reachable() {
        let base = filter("local", None, "text", &[]);
        let a = controls(vec![
            base.clone(),
            filter("type", None, "text", &[]),
            filter("row", None, "result", &[]),
        ]);
        let mut different = base.clone();
        different.label = "Different".into();
        let b = controls(vec![
            different,
            filter("kind", Some("type"), "options", &["article"]),
            filter("b_local", None, "toggle", &[]),
        ]);
        let c = a.clone();
        let merged = merge(&[("a", "A", &a), ("b", "B", &b), ("c", "C", &c)]);
        let names: Vec<_> = merged
            .controls
            .filters
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(names, ["local", "a_type", "b_local_2", "type", "b_local"]);
        let own = merged.project(
            "a",
            &BrowseComposition {
                filters: vec![("a_type".into(), vec!["paper".into()])],
                ..Default::default()
            },
        );
        assert_eq!(own.filters, [("type".into(), vec!["paper".into()])]);
    }

    #[test]
    fn projection_preserves_own_year_filters_and_does_not_infer_untagged_dates() {
        let a = controls(vec![
            filter("start_year", Some("year_from"), "integer", &[]),
            filter("date", Some("published_from"), "date", &[]),
        ]);
        let b = controls(vec![filter("published_from", None, "date", &[])]);
        let merged = merge(&[("a", "A", &a), ("b", "B", &b)]);
        let composed = BrowseComposition {
            filters: vec![("year_from".into(), vec!["2020".into()])],
            ..Default::default()
        };
        assert_eq!(
            merged.project("a", &composed).filters,
            [("start_year".into(), vec!["2020".into()])]
        );
        let own = merged.project("b", &composed);
        assert!(own.is_empty());
        assert_eq!(own.notes.len(), 1);
    }

    #[test]
    fn tagged_and_untagged_orders_union_and_restore_provider_values() {
        let mut a = controls(vec![]);
        a.sort = Some(SortControl {
            default: "date_desc".into(),
            keyword_default: Some("relevance".into()),
            options: vec![option("date_desc"), option("relevance"), option("local")],
            equivalent_to: BTreeMap::from([
                ("date_desc".into(), "newest".into()),
                ("relevance".into(), "most_relevant".into()),
            ]),
        });
        let mut b = controls(vec![]);
        b.sort = Some(SortControl {
            default: "recent".into(),
            keyword_default: None,
            options: vec![option("recent"), option("local"), option("other")],
            equivalent_to: BTreeMap::from([("recent".into(), "newest".into())]),
        });
        let merged = merge(&[("a", "A", &a), ("b", "B", &b)]);
        let sort = merged.controls.sort.as_ref().unwrap();
        assert_eq!(sort.default, "newest");
        assert_eq!(sort.keyword_default.as_deref(), Some("most_relevant"));
        assert_eq!(
            sort.options
                .iter()
                .map(|o| o.value.as_str())
                .collect::<Vec<_>>(),
            ["newest", "most_relevant", "local", "other"]
        );
        let composed = BrowseComposition {
            sort: Some("newest".into()),
            ..Default::default()
        };
        assert_eq!(
            merged.project("a", &composed).sort.as_deref(),
            Some("date_desc")
        );
        assert_eq!(
            merged.project("b", &composed).sort.as_deref(),
            Some("recent")
        );
        let composed = BrowseComposition {
            sort: Some("local".into()),
            ..Default::default()
        };
        assert_eq!(
            merged.project("b", &composed).sort.as_deref(),
            Some("local")
        );
    }

    #[test]
    fn inherited_capabilities_merge_to_the_bases_size() {
        let base = Manifest::parse(include_str!("builtin/openalex.toml")).unwrap();
        struct TestBase(Manifest);
        impl super::super::custom::manifest::Bases for TestBase {
            fn base(&self, _: &str) -> anyhow::Result<super::super::custom::manifest::Base> {
                Ok(super::super::custom::manifest::Base {
                    manifest: self.0.clone(),
                    secrets: Default::default(),
                })
            }
        }
        let resolved = Manifest::resolve("manifest_version = 2\nid = 'child'\nname = 'Child'\nextends = { provider = 'openalex', capabilities = ['search', 'browse'] }\n", &TestBase(base.clone())).unwrap();
        let a = base.capabilities.browse.as_ref().unwrap().controls();
        let b = resolved
            .manifest
            .capabilities
            .browse
            .as_ref()
            .unwrap()
            .controls();
        let merged = merge(&[("openalex", "OpenAlex", &a), ("child", "Child", &b)]);
        assert_eq!(merged.controls.filters.len(), a.filters.len());
        assert_eq!(
            merged.controls.sort.as_ref().unwrap().options.len(),
            a.sort.as_ref().unwrap().options.len()
        );
    }
}
