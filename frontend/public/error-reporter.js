(function() {
    var errorEndpoint = "/api/frontend-error";
    var t0 = performance.now();

    function report(errorType, message, source) {
        try {
            var body = JSON.stringify({
                error_type: errorType,
                message: message,
                source: source || "perf"
            });
            fetch(errorEndpoint, {
                method: "POST",
                headers: {"Content-Type": "application/json"},
                body: body
            }).catch(function() {});
        } catch(e) {}
    }

    // === 错误捕获 ===
    window.onerror = function(msg, src, line, col, err) {
        var source = (src || "unknown") + ":" + (line || 0) + ":" + (col || 0);
        var message = String(msg);
        if (err && err.stack) {
            message += "\n" + err.stack;
        }
        report("js_error", message, source);
    };

    window.addEventListener("unhandledrejection", function(e) {
        var message = e.reason ? String(e.reason) : "unknown rejection";
        if (e.reason && e.reason.stack) {
            message += "\n" + e.reason.stack;
        }
        report("promise_rejection", message, "unhandledrejection");
    });

    // === 性能埋点 ===

    // 1. DOMContentLoaded
    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", function() {
            report("perf", "DOMContentLoaded: " + Math.round(performance.now() - t0) + "ms");
        });
    } else {
        report("perf", "DOMContentLoaded: already (script loaded late)");
    }

    // 2. WASM资源加载监控（PerformanceObserver）
    if (window.PerformanceObserver) {
        var wasmObserver = new PerformanceObserver(function(list) {
            list.getEntries().forEach(function(entry) {
                if (entry.name.indexOf(".wasm") !== -1) {
                    var size = entry.transferSize ? (entry.transferSize / 1024).toFixed(0) + "KB" : "cached";
                    report("perf",
                        "WASM loaded: " + Math.round(entry.duration) + "ms, size=" + size +
                        ", dns=" + Math.round(entry.domainLookupEnd - entry.domainLookupStart) + "ms" +
                        ", connect=" + Math.round(entry.connectEnd - entry.connectStart) + "ms" +
                        ", download=" + Math.round(entry.responseEnd - entry.responseStart) + "ms"
                    );
                }
                if (entry.name.indexOf(".js") !== -1 && entry.name.indexOf("frontend") !== -1) {
                    report("perf",
                        "JS loaded: " + Math.round(entry.duration) + "ms, size=" +
                        (entry.transferSize ? (entry.transferSize / 1024).toFixed(0) + "KB" : "cached")
                    );
                }
            });
        });
        try {
            wasmObserver.observe({type: "resource", buffered: true});
        } catch(e) {}
    }

    // 3. Hydration完成标记（Rust端调用 window.__markHydrated()）
    var hydrateStart = null;
    window.__markHydrateStart = function() {
        hydrateStart = performance.now();
        report("perf", "Hydrate start: " + Math.round(hydrateStart - t0) + "ms after page load");
    };
    window.__markHydrated = function() {
        var now = performance.now();
        var hydrateTime = hydrateStart ? Math.round(now - hydrateStart) + "ms" : "unknown";
        report("perf", "Hydrate done: " + Math.round(now - t0) + "ms after page load, hydrate took " + hydrateTime);
        // Fallback: 如果Leptos信号未能隐藏loading overlay，5秒后用JS强制隐藏
        setTimeout(function() {
            var overlay = document.querySelector(".app-loading-overlay");
            if (overlay) {
                overlay.style.display = "none";
                report("perf", "Loading overlay force-hidden by JS fallback");
            }
        }, 5000);
    };

    // 4. 首次点击监控（检测点击是否被响应）
    var firstClickTime = null;
    var clickResponded = false;
    document.addEventListener("click", function(e) {
        if (!firstClickTime) {
            firstClickTime = performance.now();
            report("perf", "First click: " + Math.round(firstClickTime - t0) + "ms after page load, target=" + e.target.tagName + "." + (e.target.className || "").split(" ")[0]);
            // 200ms后检查是否有DOM变化（说明点击被处理了）
            var snapshot = document.querySelector(".messages") ? document.querySelector(".messages").innerHTML.length : 0;
            setTimeout(function() {
                var current = document.querySelector(".messages") ? document.querySelector(".messages").innerHTML.length : 0;
                if (current !== snapshot) {
                    report("perf", "First click responded: " + Math.round(performance.now() - firstClickTime) + "ms");
                } else {
                    report("perf", "First click NO response after 200ms (hydration not ready?)");
                }
                clickResponded = true;
            }, 200);
        }
    }, true);

    console.log("[Reporter] Error + performance reporting active");
})();

