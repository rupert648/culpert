//! Procedural macros for culpert. End users get these via
//! `culpert::span_fn` — the `culpert` crate re-exports them with the
//! correct crate path. Don't depend on this crate directly.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn, LitStr};

/// `#[culpert::span_fn("name")]` — wraps the function body in a
/// [`culpert::scope::Scope`](https://docs.rs/culpert/latest/culpert/scope/struct.Scope.html)
/// guard so allocations performed inside the function are attributed to
/// the named span via [`culpert::scope::LocalSpanContext`].
///
/// Independent of any external tracer (foundations / tracing crate). The
/// attribution works regardless of trace sampling rate, because culpert
/// owns the scope stack directly.
///
/// # Sync and async
///
/// For sync functions, the macro inserts an RAII scope guard that covers
/// the entire function body.
///
/// For async functions, the macro wraps the body in a
/// [`culpert::ScopedFuture`](https://docs.rs/culpert/latest/culpert/struct.ScopedFuture.html)
/// that enters/exits the scope around each `poll()`. The parent span is
/// captured at the call site (from the caller's thread-local scope
/// stack), so parent-child relationships are preserved even when the
/// async task migrates between executor threads.
///
/// # Example
///
/// ```ignore
/// #[culpert::span_fn("render_template")]
/// fn render_template(input: &Input) -> Output {
///     // every alloc here is attributed to span "render_template"
/// }
///
/// #[culpert::span_fn("fetch_data")]
/// async fn fetch_data(url: &str) -> Data {
///     // allocations across .await points are attributed to "fetch_data"
///     let resp = client.get(url).await;
///     resp.json().await
/// }
/// ```
#[proc_macro_attribute]
pub fn span_fn(attr: TokenStream, item: TokenStream) -> TokenStream {
    let name = parse_macro_input!(attr as LitStr);
    let func = parse_macro_input!(item as ItemFn);

    let attrs = &func.attrs;
    let vis = &func.vis;
    let sig = &func.sig;
    let block = &func.block;

    let expanded = if func.sig.asyncness.is_some() {
        // Async path: wrap the body in a ScopedFuture that enters/exits
        // the scope on each poll(). The outer function stays `async fn`
        // (preserving the original return type), and the inner
        // ScopedFuture is `.await`ed immediately — so each poll of the
        // outer future goes through ScopedFuture::poll, which pushes
        // and pops the scope guard.
        //
        // ScopedFuture::new captures the caller's current scope-stack
        // top as the parent at construction time (i.e. when the async
        // fn is first called, before any .await).
        quote! {
            #(#attrs)*
            #vis #sig {
                ::culpert::ScopedFuture::new(#name, async move #block).await
            }
        }
    } else {
        // Sync path: RAII guard held for the duration of the body.
        // Drop guarantees the pop runs on every exit path — `?`, early
        // `return`, or panic — because Drop runs as the function frame
        // unwinds.
        quote! {
            #(#attrs)*
            #vis #sig {
                let __culpert_scope = ::culpert::scope::enter(#name);
                #block
            }
        }
    };

    expanded.into()
}
