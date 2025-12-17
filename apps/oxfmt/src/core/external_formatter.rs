use std::sync::Arc;

use napi::{
    Status,
    bindgen_prelude::{FnArgs, Promise, block_on},
    threadsafe_function::ThreadsafeFunction,
};
use serde_json::Value;

/// Type alias for the setup config callback function signature.
/// Takes num_threads as argument and returns plugin languages.
pub type JsSetupConfigCb = ThreadsafeFunction<
    // Input arguments
    FnArgs<(u32,)>, // (num_threads,)
    // Return type (what JS function returns)
    Promise<Vec<String>>,
    // Arguments (repeated)
    FnArgs<(u32,)>,
    // Error status
    Status,
    // CalleeHandled
    false,
>;

/// Type alias for the callback function signature.
/// Takes (options, tag_name, code) as separate arguments and returns formatted code.
pub type JsFormatEmbeddedCb = ThreadsafeFunction<
    // Input arguments
    FnArgs<(Value, String, String)>, // (options, tag_name, code)
    // Return type (what JS function returns)
    Promise<String>,
    // Arguments (repeated)
    FnArgs<(Value, String, String)>,
    // Error status
    Status,
    // CalleeHandled
    false,
>;

/// Type alias for the callback function signature.
/// Takes (options, parser_name, file_name, code) as separate arguments and returns formatted code.
pub type JsFormatFileCb = ThreadsafeFunction<
    // Input arguments
    FnArgs<(Value, String, String, String)>, // (options, parser_name, file_name, code)
    // Return type (what JS function returns)
    Promise<String>,
    // Arguments (repeated)
    FnArgs<(Value, String, String, String)>,
    // Error status
    Status,
    // CalleeHandled
    false,
>;

/// Callback function type for formatting embedded code with config.
/// Takes (options, tag_name, code) and returns formatted code or an error.
type FormatEmbeddedWithConfigCallback =
    Arc<dyn Fn(&Value, &str, &str) -> Result<String, String> + Send + Sync>;

/// Callback function type for formatting files with config.
/// Takes (options, parser_name, file_name, code) and returns formatted code or an error.
type FormatFileWithConfigCallback =
    Arc<dyn Fn(&Value, &str, &str, &str) -> Result<String, String> + Send + Sync>;

/// Callback function type for setup config.
/// Takes num_threads and returns plugin languages.
type SetupConfigCallback = Arc<dyn Fn(usize) -> Result<Vec<String>, String> + Send + Sync>;

/// External formatter that wraps a JS callback.
#[derive(Clone)]
pub struct ExternalFormatter {
    pub setup_config: SetupConfigCallback,
    pub format_embedded: FormatEmbeddedWithConfigCallback,
    pub format_file: FormatFileWithConfigCallback,
}

impl std::fmt::Debug for ExternalFormatter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalFormatter")
            .field("setup_config", &"<callback>")
            .field("format_embedded", &"<callback>")
            .field("format_file", &"<callback>")
            .finish()
    }
}

impl ExternalFormatter {
    /// Create an [`ExternalFormatter`] from JS callbacks.
    pub fn new(
        setup_config_cb: JsSetupConfigCb,
        format_embedded_cb: JsFormatEmbeddedCb,
        format_file_cb: JsFormatFileCb,
    ) -> Self {
        let rust_setup_config = wrap_setup_config(setup_config_cb);
        let rust_format_embedded = wrap_format_embedded(format_embedded_cb);
        let rust_format_file = wrap_format_file(format_file_cb);
        Self {
            setup_config: rust_setup_config,
            format_embedded: rust_format_embedded,
            format_file: rust_format_file,
        }
    }

    /// Setup worker pool using the JS callback.
    pub fn setup_config(&self, num_threads: usize) -> Result<Vec<String>, String> {
        (self.setup_config)(num_threads)
    }

    /// Convert this external formatter to the oxc_formatter::EmbeddedFormatter type.
    /// The options is captured in the closure and passed to JS on each call.
    pub fn to_embedded_formatter(&self, options: Value) -> oxc_formatter::EmbeddedFormatter {
        let format_embedded = Arc::clone(&self.format_embedded);
        let callback = Arc::new(move |tag_name: &str, code: &str| {
            (format_embedded)(&options, tag_name, code)
        });
        oxc_formatter::EmbeddedFormatter::new(callback)
    }

    /// Format non-js file using the JS callback.
    pub fn format_file(
        &self,
        options: &Value,
        parser_name: &str,
        file_name: &str,
        code: &str,
    ) -> Result<String, String> {
        (self.format_file)(options, parser_name, file_name, code)
    }
}

// ---

// NOTE: These methods are all wrapped by `block_on` to run the async JS calls in a blocking manner.
//
// When called from `rayon` worker threads (Mode::Cli), this works fine.
// Because `rayon` threads are separate from the `tokio` runtime.
//
// However, in cases like `--stdin-filepath` or Node.js API calls,
// where already inside an async context (the `napi`'s `async` function),
// calling `block_on` directly would cause issues with nested async runtime access.
//
// Therefore, `block_in_place()` is used at the call site
// to temporarily convert the current async task into a blocking context.

/// Wrap JS `setupConfig` callback as a normal Rust function.
fn wrap_setup_config(cb: JsSetupConfigCb) -> SetupConfigCallback {
    Arc::new(move |num_threads: usize| {
        block_on(async {
            #[expect(clippy::cast_possible_truncation)]
            let status = cb.call_async(FnArgs::from((num_threads as u32,))).await;
            match status {
                Ok(promise) => match promise.await {
                    Ok(languages) => Ok(languages),
                    Err(err) => Err(format!("JS setupConfig promise rejected: {err}")),
                },
                Err(err) => Err(format!("Failed to call JS setupConfig callback: {err}")),
            }
        })
    })
}

/// Wrap JS `formatEmbeddedCode` callback as a normal Rust function.
fn wrap_format_embedded(cb: JsFormatEmbeddedCb) -> FormatEmbeddedWithConfigCallback {
    Arc::new(move |options: &Value, tag_name: &str, code: &str| {
        block_on(async {
            let status = cb
                .call_async(FnArgs::from((
                    options.clone(),
                    tag_name.to_string(),
                    code.to_string(),
                )))
                .await;
            match status {
                Ok(promise) => match promise.await {
                    Ok(formatted_code) => Ok(formatted_code),
                    Err(err) => {
                        Err(format!("JS formatter promise rejected for tag '{tag_name}': {err}"))
                    }
                },
                Err(err) => Err(format!(
                    "Failed to call JS formatting callback for tag '{tag_name}': {err}"
                )),
            }
        })
    })
}

/// Wrap JS `formatFile` callback as a normal Rust function.
fn wrap_format_file(cb: JsFormatFileCb) -> FormatFileWithConfigCallback {
    Arc::new(move |options: &Value, parser_name: &str, file_name: &str, code: &str| {
        block_on(async {
            let status = cb
                .call_async(FnArgs::from((
                    options.clone(),
                    parser_name.to_string(),
                    file_name.to_string(),
                    code.to_string(),
                )))
                .await;
            match status {
                Ok(promise) => match promise.await {
                    Ok(formatted_code) => Ok(formatted_code),
                    Err(err) => Err(format!(
                        "JS formatFile promise rejected for file: '{file_name}', parser: '{parser_name}': {err}"
                    )),
                },
                Err(err) => Err(format!(
                    "Failed to call JS formatFile callback for file: '{file_name}', parser: '{parser_name}': {err}"
                )),
            }
        })
    })
}
