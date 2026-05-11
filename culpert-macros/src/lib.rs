//! Procedural macros for culpert. End users get these via
//! `culpert::span_fn` — the `culpert` crate re-exports them with the
//! correct crate path. Don't depend on this crate directly.

use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemFn, LitStr, parse_macro_input};

/// `#[culpert::span_fn("name")]` — wraps the function body in a
/// [`culpert::scope::Scope`](https://docs.rs/culpert/latest/culpert/scope/struct.Scope.html)
/// guard so allocations performed inside the function are attributed to
/// the named span via [`culpert::scope::LocalSpanContext`].
///
/// Independent of any external tracer (foundations / tracing crate). The
/// attribution works regardless of trace sampling rate, because culpert
/// owns the scope stack directly.
///
/// # Sync only in v0.2
///
/// Applied to an `async fn`, the macro emits a compile error pointing at
/// the alternatives. Async support needs a `ScopedFuture` wrapper with
/// careful parent-capture semantics across `.await` points; it's queued
/// for a follow-up release.
///
/// # Example
///
/// ```ignore
/// #[culpert::span_fn("render_template")]
/// fn render_template(input: &Input) -> Output {
///     // every alloc here is attributed to span "render_template"
/// }
/// ```
#[proc_macro_attribute]
pub fn span_fn(attr: TokenStream, item: TokenStream) -> TokenStream {
    let name = parse_macro_input!(attr as LitStr);
    let func = parse_macro_input!(item as ItemFn);

    // Async support deferred — see crate docs above.
    if let Some(async_token) = func.sig.asyncness {
        return syn::Error::new(
            async_token.span,
            "#[culpert::span_fn] does not yet support async fns.\n\
             Workarounds:\n\
              - For foundations services, use #[foundations::telemetry::tracing::span_fn]\n\
                and configure culpert via culpert-foundations — async propagation works\n\
                through foundations' WithTelemetryContext::poll.\n\
              - For tracing-crate services, use #[tracing::instrument] and configure\n\
                culpert via culpert-tracing.\n\
              - For sync-only inner functions, factor the work out of the async body\n\
                and annotate the sync fn with #[culpert::span_fn].",
        )
        .to_compile_error()
        .into();
    }

    let attrs = &func.attrs;
    let vis = &func.vis;
    let sig = &func.sig;
    let block = &func.block;

    let expanded = quote! {
        #(#attrs)*
        #vis #sig {
            // Held for the duration of the body. RAII guarantees the
            // pop runs on every exit path — `?`, early `return`, or
            // panic — because Drop runs as the function frame unwinds.
            let __culpert_scope = ::culpert::scope::enter(#name);
            #block
        }
    };

    expanded.into()
}
