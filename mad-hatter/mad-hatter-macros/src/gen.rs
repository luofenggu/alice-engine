use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Type;

use crate::parse::{EndpointDef, HttpMethod, ParamDef, ServiceDef};

pub fn generate(def: ServiceDef) -> TokenStream {
    let service_name = &def.name;
    let trait_name = format_ident!("{}Service", service_name);
    let routes_mod = format_ident!("{}_routes", to_snake_case(&service_name.to_string()));

    // 1. Generate route constants
    let route_consts = generate_route_consts(&def.endpoints);

    // 2. Generate service trait methods
    let trait_methods = generate_trait_methods(&def.endpoints);

    // 3. Generate handler functions (top-level, outside impl block)
    let handler_fns: Vec<TokenStream> = def
        .endpoints
        .iter()
        .map(|ep| generate_handler(ep, &trait_name))
        .collect();

    // 4. Generate route registrations
    let route_calls: Vec<TokenStream> = def
        .endpoints
        .iter()
        .map(|ep| {
            let const_name = format_ident!("{}", ep.fn_name.to_string().to_uppercase());
            let handler_name = format_ident!("__handle_{}", ep.fn_name);
            let method_fn = match ep.method {
                HttpMethod::Get => quote! { ::axum::routing::get },
                HttpMethod::Post => quote! { ::axum::routing::post },
                HttpMethod::Put => quote! { ::axum::routing::put },
                HttpMethod::Delete => quote! { ::axum::routing::delete },
                HttpMethod::Patch => quote! { ::axum::routing::patch },
            };
            quote! {
                .route(#routes_mod::#const_name, #method_fn(#handler_name::<S>))
            }
        })
        .collect();

    quote! {
        /// Route path constants (auto-generated)
        pub mod #routes_mod {
            #(#route_consts)*
        }

        /// Service trait (auto-generated). Implement this for your application state.
        pub trait #trait_name: Send + Sync + 'static {
            #(#trait_methods)*
        }

        // Handler functions (auto-generated, not part of public API)
        #(#handler_fns)*

        /// Service definition (auto-generated).
        pub struct #service_name;

        impl #service_name {
            /// Build an axum Router with the given state type.
            ///
            /// Returns `Router<Arc<S>>` — the caller is responsible for
            /// calling `.with_state(state)` when ready.
            pub fn router<S: #trait_name>() -> ::axum::Router<::std::sync::Arc<S>> {
                ::axum::Router::new()
                    #(#route_calls)*
            }
        }
    }
}

fn generate_route_consts(endpoints: &[EndpointDef]) -> Vec<TokenStream> {
    endpoints
        .iter()
        .map(|ep| {
            let const_name = format_ident!("{}", ep.fn_name.to_string().to_uppercase());
            // axum 0.8+ uses {param} natively, no conversion needed
            let axum_path = format!("/{}", &ep.path);
            quote! {
                pub const #const_name: &str = #axum_path;
            }
        })
        .collect()
}

fn generate_trait_methods(endpoints: &[EndpointDef]) -> Vec<TokenStream> {
    endpoints
        .iter()
        .map(|ep| {
            let fn_name = &ep.fn_name;
            let params = trait_params(&ep.params);
            let ret = match &ep.return_type {
                Some(ty) => {
                    let qualified = qualify_known_types(ty);
                    quote! { ::std::result::Result<#qualified, ::mad_hatter::HttpError> }
                }
                None => quote! { ::std::result::Result<(), ::mad_hatter::HttpError> },
            };
            quote! {
                fn #fn_name(&self, #(#params),*) -> impl ::std::future::Future<Output = #ret> + Send;
            }
        })
        .collect()
}

fn generate_handler(ep: &EndpointDef, trait_name: &proc_macro2::Ident) -> TokenStream {
    let handler_name = format_ident!("__handle_{}", ep.fn_name);
    let fn_name = &ep.fn_name;

    // Separate path params from body params
    let path_params: Vec<&ParamDef> = ep
        .params
        .iter()
        .filter(|p| p.name != "body")
        .collect();
    let body_param: Option<&ParamDef> = ep.params.iter().find(|p| p.name == "body");

    // Build extractor parameters for the handler
    let mut handler_params = Vec::new();
    let mut call_args = Vec::new();

    // State is always first
    handler_params.push(quote! {
        ::axum::extract::State(svc): ::axum::extract::State<::std::sync::Arc<S>>
    });

    // Path parameters
    if path_params.len() == 1 {
        let ty = &path_params[0].ty;
        let name = &path_params[0].name;
        handler_params.push(quote! {
            ::axum::extract::Path(#name): ::axum::extract::Path<#ty>
        });
        call_args.push(quote! { #name });
    } else if path_params.len() > 1 {
        let types: Vec<&Type> = path_params.iter().map(|p| &p.ty).collect();
        let names: Vec<&proc_macro2::Ident> = path_params.iter().map(|p| &p.name).collect();
        let tuple_names = quote! { (#(#names),*) };
        let tuple_types = quote! { (#(#types),*) };
        handler_params.push(quote! {
            ::axum::extract::Path(#tuple_names): ::axum::extract::Path<#tuple_types>
        });
        for name in &names {
            call_args.push(quote! { #name });
        }
    }

    // Body parameter
    if let Some(bp) = body_param {
        let ty = &bp.ty;
        handler_params.push(quote! {
            ::axum::Json(body): ::axum::Json<#ty>
        });
        call_args.push(quote! { body });
    }

    // Return type handling
    let response_conversion = if ep.return_type.is_some() {
        quote! {
            match svc.#fn_name(#(#call_args),*).await {
                Ok(val) => ::axum::response::IntoResponse::into_response(val),
                Err(err) => ::axum::response::IntoResponse::into_response(err),
            }
        }
    } else {
        quote! {
            match svc.#fn_name(#(#call_args),*).await {
                Ok(()) => ::axum::response::IntoResponse::into_response(::axum::http::StatusCode::NO_CONTENT),
                Err(err) => ::axum::response::IntoResponse::into_response(err),
            }
        }
    };

    quote! {
        async fn #handler_name<S: #trait_name>(
            #(#handler_params),*
        ) -> ::axum::response::Response {
            #response_conversion
        }
    }
}

/// Qualify known framework types used in macro definitions.
///
/// Converts bare `Json<T>` to `::axum::Json<T>` so that generated code
/// doesn't require the caller to have `use axum::Json` in scope.
fn qualify_known_types(ty: &Type) -> TokenStream {
    if let Type::Path(tp) = ty {
        if tp.qself.is_none() && tp.path.segments.len() == 1 {
            let seg = &tp.path.segments[0];
            if seg.ident == "Json" {
                // Extract the generic arguments (e.g., <User>)
                let args = &seg.arguments;
                return quote! { ::axum::Json #args };
            }
        }
    }
    // For any other type, emit as-is
    quote! { #ty }
}

fn trait_params(params: &[ParamDef]) -> Vec<TokenStream> {
    params
        .iter()
        .map(|p| {
            let name = &p.name;
            let ty = &p.ty;
            quote! { #name: #ty }
        })
        .collect()
}

fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(ch.to_lowercase().next().unwrap());
        } else {
            result.push(ch);
        }
    }
    result
}
