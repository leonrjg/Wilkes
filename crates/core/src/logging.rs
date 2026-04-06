use std::collections::VecDeque;
use std::sync::LazyLock;
use parking_lot::Mutex;
use tracing_subscriber::{fmt, prelude::*, EnvFilter, Layer};

const MAX_LOG_LINES: usize = 1000;

static LOG_BUFFER: LazyLock<Mutex<VecDeque<String>>> = LazyLock::new(|| {
    Mutex::new(VecDeque::with_capacity(MAX_LOG_LINES))
});

pub struct BufferLayer;

impl<S> Layer<S> for BufferLayer
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut buffer = LOG_BUFFER.lock();
        if buffer.len() >= MAX_LOG_LINES {
            buffer.pop_front();
        }

        // Format the event. For simplicity, we just use the event's metadata and fields.
        let mut msg = String::new();
        
        let metadata = event.metadata();
        msg.push_str(&format!(
            "[{}] {:<5} {}: ",
            chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f"),
            metadata.level().to_string(),
            metadata.target(),
        ));

        struct MsgVisitor<'a>(&'a mut String);
        impl<'a> tracing::field::Visit for MsgVisitor<'a> {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0.push_str(&format!("{:?}", value));
                } else {
                    self.0.push_str(&format!(" {}={:?}", field.name(), value));
                }
            }
        }

        event.record(&mut MsgVisitor(&mut msg));
        buffer.push_back(msg);
    }
}

pub fn init_logging() {
    let fmt_layer = fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_thread_names(false);

    let filter_layer = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info,hf_hub=warn"))
        .unwrap();

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .with(BufferLayer)
        .init();

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("PANIC: {info}");
        default_hook(info);
    }));
}

/// Like `init_logging`, but writes to stderr instead of stdout.
/// Must be used by worker subprocesses whose stdout is a JSON protocol channel.
pub fn init_logging_stderr() {
    let fmt_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_thread_ids(false)
        .with_thread_names(false);

    let filter_layer = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info,hf_hub=warn"))
        .unwrap();

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .init();
}

pub fn get_logs() -> Vec<String> {
    LOG_BUFFER.lock().iter().cloned().collect()
}

pub fn clear_logs() {
    LOG_BUFFER.lock().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::{info, subscriber};

    #[test]
    fn test_logging_buffer() {
        // Since we can't easily re-init global logging in tests if it was already inited,
        // we can at least test the buffer and layer logic.
        let layer = BufferLayer;
        
        // Use a local subscriber with our layer for testing
        let subscriber = tracing_subscriber::registry().with(layer);
        
        subscriber::with_default(subscriber, || {
            info!("test message 123");
            info!(my_field="my_value", "test message with fields");
        });

        let logs = get_logs();
        assert!(logs.iter().any(|l| l.contains("test message 123")));
        assert!(logs.iter().any(|l| l.contains("test message with fields") && l.contains("my_field=\"my_value\"")));

        clear_logs();
        assert_eq!(get_logs().len(), 0);
    }
}
