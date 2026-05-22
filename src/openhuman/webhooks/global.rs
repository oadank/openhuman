use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use super::WebhookRouter;

static GLOBAL_WEBHOOK_ROUTER: OnceLock<Arc<WebhookRouter>> = OnceLock::new();

pub fn init_global_webhook_router(persist_path: Option<PathBuf>) -> Arc<WebhookRouter> {
    let router = Arc::new(WebhookRouter::new(persist_path));
    match GLOBAL_WEBHOOK_ROUTER.set(Arc::clone(&router)) {
        Ok(()) => router,
        Err(_) => GLOBAL_WEBHOOK_ROUTER
            .get()
            .expect("global webhook router initialized")
            .clone(),
    }
}

pub fn global_webhook_router() -> Option<Arc<WebhookRouter>> {
    GLOBAL_WEBHOOK_ROUTER.get().cloned()
}
