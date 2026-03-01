pub mod app;
#[cfg(feature = "ssr")]
pub mod api;

#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    use leptos::prelude::*;
    console_error_panic_hook::set_once();

    // 通知JS：hydration开始
    let _ = js_sys::eval("window.__hydrateStart = performance.now()");

    leptos::mount::hydrate_body(app::App);

    // 通知JS：hydration完成，触发回调
    let _ = js_sys::eval("window.__markHydrated && window.__markHydrated()");
}
