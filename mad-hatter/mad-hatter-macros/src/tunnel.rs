use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{parse2, ItemTrait, TraitItem, FnArg, ReturnType, Type, PathArguments, GenericArgument};

pub fn derive_tunnel_service(input: TokenStream) -> TokenStream {
    let trait_def: ItemTrait = match parse2(input) {
        Ok(t) => t,
        Err(e) => return e.to_compile_error(),
    };

    let trait_name = &trait_def.ident;
    let trait_name_str = trait_name.to_string();
    let proxy_name = format_ident!("{}Proxy", trait_name);
    let dispatcher_name = format_ident!("{}Dispatcher", trait_name);

    // Collect method info
    let mut methods = Vec::new();
    for item in &trait_def.items {
        if let TraitItem::Fn(method) = item {
            let method_name = &method.sig.ident;
            let method_name_str = method_name.to_string();
            let full_method_name = format!("{}.{}", trait_name_str, method_name_str);

            // Validate: must have &self receiver
            let has_self = method.sig.inputs.iter().any(|arg| matches!(arg, FnArg::Receiver(_)));
            if !has_self {
                return syn::Error::new_spanned(
                    &method.sig,
                    format!("#[tunnel_service] method '{}' must have &self receiver", method_name_str),
                ).to_compile_error();
            }

            // Extract return type — must be Result<T, String>
            let ok_type = match extract_result_ok_type(&method.sig.output) {
                Some(t) => t,
                None => {
                    return syn::Error::new_spanned(
                        &method.sig.output,
                        format!("#[tunnel_service] method '{}' must return Result<T, String>", method_name_str),
                    ).to_compile_error();
                }
            };

            // Extract parameters (skip &self)
            let params: Vec<_> = method.sig.inputs.iter().filter_map(|arg| {
                if let FnArg::Typed(pat_type) = arg {
                    Some(pat_type)
                } else {
                    None
                }
            }).collect();

            let param_names: Vec<_> = params.iter().map(|p| &p.pat).collect();
            let param_types: Vec<_> = params.iter().map(|p| &p.ty).collect();

            methods.push(MethodInfo {
                method_name: method_name.clone(),
                full_method_name,
                ok_type: ok_type.clone(),
                param_names: param_names.into_iter().cloned().collect(),
                param_types: param_types.into_iter().cloned().collect(),
            });
        }
    }

    // Generate Proxy impl
    let proxy_methods: Vec<_> = methods.iter().map(|m| {
        let method_name = &m.method_name;
        let full_name = &m.full_method_name;
        let ok_type = &m.ok_type;
        let param_names = &m.param_names;
        let _param_types = &m.param_types;

        // Build parameter list for trait method signature
        let params_sig: Vec<_> = m.param_names.iter().zip(m.param_types.iter()).map(|(name, ty)| {
            quote! { #name: #ty }
        }).collect();

        // Serialize params as tuple
        let serialize_params = if param_names.is_empty() {
            quote! { serde_json::Value::Null }
        } else if param_names.len() == 1 {
            let name = &param_names[0];
            quote! { serde_json::to_value(&#name).map_err(|e| format!("serialize error: {}", e))? }
        } else {
            let names = param_names.iter().map(|n| quote! { &#n });
            quote! { serde_json::to_value((#(#names),*)).map_err(|e| format!("serialize error: {}", e))? }
        };

        quote! {
            fn #method_name(&self, #(#params_sig),*) -> Result<#ok_type, String> {
                let params = #serialize_params;
                let result = self.__endpoint.call(#full_name, params)?;
                serde_json::from_value(result).map_err(|e| format!("deserialize error: {}", e))
            }
        }
    }).collect();

    // Generate Dispatcher match arms
    let dispatch_arms: Vec<_> = methods.iter().map(|m| {
        let method_name = &m.method_name;
        let full_name = &m.full_method_name;
        let param_types = &m.param_types;

        let deserialize_and_call = if m.param_names.is_empty() {
            quote! {
                let result = self.__handler.#method_name()?;
                serde_json::to_value(result).map_err(|e| format!("serialize result error: {}", e))
            }
        } else if m.param_names.len() == 1 {
            let ty = &param_types[0];
            quote! {
                let arg: #ty = serde_json::from_value(params)
                    .map_err(|e| format!("deserialize params error: {}", e))?;
                let result = self.__handler.#method_name(arg)?;
                serde_json::to_value(result).map_err(|e| format!("serialize result error: {}", e))
            }
        } else {
            let types = param_types.iter();
            let indices: Vec<_> = (0..param_types.len()).map(syn::Index::from).collect();
            quote! {
                let args: (#(#types),*) = serde_json::from_value(params)
                    .map_err(|e| format!("deserialize params error: {}", e))?;
                let result = self.__handler.#method_name(#(args.#indices),*)?;
                serde_json::to_value(result).map_err(|e| format!("serialize result error: {}", e))
            }
        };

        quote! {
            #full_name => { #deserialize_and_call }
        }
    }).collect();

    let output = quote! {
        // Original trait preserved
        #trait_def

        // Proxy: implements the trait by forwarding calls through tunnel
        pub struct #proxy_name {
            __endpoint: std::sync::Arc<mad_hatter::tunnel::TunnelEndpoint>,
        }

        impl #proxy_name {
            pub fn new(endpoint: std::sync::Arc<mad_hatter::tunnel::TunnelEndpoint>) -> Self {
                Self { __endpoint: endpoint }
            }
        }

        impl #trait_name for #proxy_name {
            #(#proxy_methods)*
        }

        // Dispatcher: routes incoming calls to local handler
        pub struct #dispatcher_name<T: #trait_name + Send + Sync> {
            __handler: std::sync::Arc<T>,
        }

        impl<T: #trait_name + Send + Sync> #dispatcher_name<T> {
            pub fn new(handler: std::sync::Arc<T>) -> Self {
                Self { __handler: handler }
            }

            pub fn boxed(handler: std::sync::Arc<T>) -> Box<dyn mad_hatter::tunnel::Dispatch>
            where T: 'static
            {
                Box::new(Self::new(handler))
            }
        }

        impl<T: #trait_name + Send + Sync> mad_hatter::tunnel::Dispatch for #dispatcher_name<T> {
            fn dispatch(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
                match method {
                    #(#dispatch_arms)*
                    _ => Err(format!("unknown method: {}", method)),
                }
            }
        }
    };

    output
}

struct MethodInfo {
    method_name: syn::Ident,
    full_method_name: String,
    ok_type: Type,
    param_names: Vec<Box<syn::Pat>>,
    param_types: Vec<Box<Type>>,
}

/// Extract the T from Result<T, String>
fn extract_result_ok_type(output: &ReturnType) -> Option<&Type> {
    let ty = match output {
        ReturnType::Type(_, ty) => ty.as_ref(),
        ReturnType::Default => return None,
    };

    // Match Path type "Result<T, String>"
    if let Type::Path(type_path) = ty {
        let segment = type_path.path.segments.last()?;
        if segment.ident != "Result" {
            return None;
        }
        if let PathArguments::AngleBracketed(args) = &segment.arguments {
            if args.args.len() != 2 {
                return None;
            }
            // First arg is T (ok type)
            if let GenericArgument::Type(ok_ty) = &args.args[0] {
                // Second arg should be String
                if let GenericArgument::Type(Type::Path(err_path)) = &args.args[1] {
                    if err_path.path.segments.last().map(|s| s.ident == "String").unwrap_or(false) {
                        return Some(ok_ty);
                    }
                }
                return None;
            }
        }
    }
    None
}