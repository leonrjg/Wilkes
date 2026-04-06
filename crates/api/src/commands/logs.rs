pub fn get_logs() -> Vec<String> {
    wilkes_core::logging::get_logs()
}

pub fn clear_logs() {
    wilkes_core::logging::clear_logs();
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;

    #[test]
    fn test_logs_api() {
        let layer = wilkes_core::logging::BufferLayer;
        let subscriber = tracing_subscriber::registry().with(layer);
        
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("test log message");
        });

        let logs = get_logs();
        assert!(logs.iter().any(|l| l.contains("test log message")));

        clear_logs();
        assert_eq!(get_logs().len(), 0);
    }
}
