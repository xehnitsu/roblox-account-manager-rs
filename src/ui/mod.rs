pub mod account_list;
pub mod add_account;
pub mod server_browser;
pub mod sessions;
pub mod settings;

use std::future::Future;
use std::sync::{Arc, Mutex};

/// Result slot for a background task started from the UI. The UI polls
/// `take()` each frame; the task wakes the UI when it finishes.
pub struct Pending<T>(Arc<Mutex<Option<T>>>);

impl<T: Send + 'static> Pending<T> {
    pub fn spawn(shared: &crate::app::Shared, fut: impl Future<Output = T> + Send + 'static) -> Self {
        let slot = Arc::new(Mutex::new(None));
        let slot2 = slot.clone();
        let ctx = shared.ctx.clone();
        shared.rt.spawn(async move {
            let v = fut.await;
            *slot2.lock().unwrap() = Some(v);
            ctx.request_repaint();
        });
        Self(slot)
    }

    pub fn take(&self) -> Option<T> {
        self.0.lock().unwrap().take()
    }
}

/// Polls an optional pending task; returns its result once and clears the slot.
pub fn poll<T: Send + 'static>(slot: &mut Option<Pending<T>>) -> Option<T> {
    let v = slot.as_ref()?.take();
    if v.is_some() {
        *slot = None;
    }
    v
}

pub fn error_label(ui: &mut egui::Ui, msg: &str) {
    ui.colored_label(ui.visuals().error_fg_color, msg);
}

/// HTTP image loader that lowercases the response MIME type. Roblox's CDN
/// serves thumbnails as `image/Png`, which the image loader doesn't recognise.
pub struct HttpLoader(egui_extras::loaders::http_loader::EhttpLoader);

impl HttpLoader {
    pub fn install(ctx: &egui::Context) {
        ctx.add_bytes_loader(Arc::new(Self(Default::default())));
    }
}

impl egui::load::BytesLoader for HttpLoader {
    fn id(&self) -> &str {
        egui::generate_loader_id!(HttpLoader)
    }

    fn load(&self, ctx: &egui::Context, uri: &str) -> egui::load::BytesLoadResult {
        match self.0.load(ctx, uri)? {
            egui::load::BytesPoll::Ready { size, bytes, mime } => {
                Ok(egui::load::BytesPoll::Ready { size, bytes, mime: mime.map(|m| m.to_ascii_lowercase()) })
            }
            pending => Ok(pending),
        }
    }

    fn forget(&self, uri: &str) {
        self.0.forget(uri);
    }

    fn forget_all(&self) {
        self.0.forget_all();
    }

    fn byte_size(&self) -> usize {
        self.0.byte_size()
    }

    fn has_pending(&self) -> bool {
        self.0.has_pending()
    }
}
