// SPDX-License-Identifier: MPL-2.0

//! Desktop loopback delivery. Only the bounded account worker waits here.
use base64::Engine as _;
use koru_account_runtime::account::{Cancel, ServiceError};
use koru_account_runtime::delivery::{Callback, SignInDelivery, SignInSession};
use koru_oauth_loopback::{CallbackListener, TwoStageCallbackListener, WaitOptions};
use std::io::Cursor;
use std::sync::OnceLock;
use std::time::Duration;
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
pub struct LoopbackDelivery;
struct Session {
    listener: TwoStageCallbackListener,
    port: u16,
}
impl SignInDelivery for LoopbackDelivery {
    fn bind(&self) -> Result<Box<dyn SignInSession>, ServiceError> {
        let listener = CallbackListener::bind(koru_account_wire::NATIVE_LOOPBACK_CALLBACK_PATH)
            .map_err(|_| ServiceError::terminal("callback_unavailable"))?;
        let port = listener.local_addr().port();
        Ok(Box::new(Session {
            listener: listener.into_two_stage(),
            port,
        }))
    }
}
impl SignInSession for Session {
    fn callback_port(&self) -> u16 {
        self.port
    }
    fn wait(&mut self, state: &str, cancel: &Cancel) -> Result<Callback, ServiceError> {
        self.wait_with(state, &|| cancel.cancelled())
    }
}
impl Session {
    fn wait_with(
        &mut self,
        state: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Callback, ServiceError> {
        let callback = self
            .listener
            .wait_stage(&wait_options(state), cancelled)
            .map_err(map_callback_error)?;
        Ok(Callback::new(
            callback
                .fields()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
        ))
    }
}
const EMBEDDED_LOGO_PNG: &[u8] = include_bytes!("../../../../assets/logo.png");

fn logo_data_uri() -> &'static str {
    static URI: OnceLock<String> = OnceLock::new();
    URI.get_or_init(|| {
        let mut encoded = Vec::new();
        if let Ok(img) = image::load_from_memory(EMBEDDED_LOGO_PNG) {
            let resized = img.resize_exact(96, 96, image::imageops::FilterType::Lanczos3);
            if resized
                .write_to(&mut Cursor::new(&mut encoded), image::ImageFormat::Png)
                .is_ok()
            {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&encoded);
                return format!("data:image/png;base64,{b64}");
            }
        }
        String::new()
    })
}

fn callback_page(message: &str, auto_close: bool) -> String {
    let logo = logo_data_uri();
    let img = if logo.is_empty() {
        String::new()
    } else {
        format!(r#"<img src="{logo}" width="48" height="48" alt="Koru">"#)
    };
    let script = if auto_close {
        "<script>window.close();</script>"
    } else {
        ""
    };
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Return to Koru</title><style>:root{{color-scheme:light dark}}*{{box-sizing:border-box}}body{{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;padding:24px;font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,Helvetica,Arial,sans-serif;background:#fafaf9;color:#1c1917}}main{{display:flex;flex-direction:column;align-items:center;text-align:center;gap:16px;max-width:360px}}img{{display:block;width:48px;height:48px;border-radius:12px}}p{{margin:0;font-size:15px;line-height:1.5;color:#57534e}}@media (prefers-color-scheme:dark){{body{{background:#141413;color:#f5f5f4}}p{{color:#a8a29e}}}}</style></head><body><main>{img}<p>{message}</p></main>{script}</body></html>"#
    )
}

fn callback_success_html() -> &'static str {
    static HTML: OnceLock<String> = OnceLock::new();
    HTML.get_or_init(|| {
        callback_page(
            "Sign-in was returned to Koru. You can close this tab.",
            true,
        )
    })
}

fn callback_error_html() -> &'static str {
    static HTML: OnceLock<String> = OnceLock::new();
    HTML.get_or_init(|| {
        callback_page(
            "Koru could not accept this sign-in. Return to Koru and try again.",
            false,
        )
    })
}

fn wait_options(state: &str) -> WaitOptions<'_> {
    WaitOptions {
        expected_state: state,
        timeout: CALLBACK_TIMEOUT,
        poll: koru_oauth_loopback::DEFAULT_POLL,
        max_request_bytes: koru_oauth_loopback::DEFAULT_MAX_REQUEST_BYTES,
        success_html: callback_success_html(),
        error_html: callback_error_html(),
    }
}
fn map_callback_error(error: koru_oauth_loopback::Error) -> ServiceError {
    match error.kind() {
        koru_oauth_loopback::ErrorKind::Cancelled => ServiceError::terminal("cancelled"),
        koru_oauth_loopback::ErrorKind::TimedOut => ServiceError::terminal("callback_timeout"),
        _ => ServiceError::terminal("callback_invalid"),
    }
}

#[cfg(test)]
mod tests;
