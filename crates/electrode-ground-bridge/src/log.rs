pub(crate) fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "electrode_ground_bridge=info,tower_http=info".into());
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
