use proc_macro::TokenStream;
use quote::{quote, format_ident};
use syn::{parse_macro_input, ItemTrait, TraitItem, FnArg, ReturnType, Type, PathArguments, GenericArgument};

pub fn derive_tunnel_service(input: TokenStream) -> TokenStream {
    let input_trait = parse_macro_input!(input as ItemTrait);
    let trait_name = &input_trait.ident;
    let trait_vis = &input_trait.vis;
    let service_name_str = trait_name.to_string();

    let proxy_name = format_ident!("{}Proxy", trait_name);
    let dispatcher_name = format_ident!("{}Dispatcher", trait_name);

    // Collect method info
    let mut methods = Vec::new();
    for item in &input_trait.items {
        if let TraitItem::Fn(method) = item {
            let method_name = &method.sig.ident;
            let method_name_str = method_name.to_string();
            let full_method_name = format!("{}.{}", service_name_str, method_name_str);

            // Extract return type: Result<T, String> -> T
            let ok_type = extract_result_ok_type(&method.sig.output);

            // Extract parameters (skip &self)
            let mut param_names = Vec::new();
            let mut param_types = Vec::new();
            for arg in &method.sig.inputs {
                if let FnArg::Typed(pat_type) = arg {
                    param_names.push(&*pat_type.pat);
                    param_types.push(&*pat_type.ty);
                }
            }

            methods.push(MethodInfo {
                method_name: method_name.clone(),
                full_method_name,
                ok_type,
                param_names,
                param_types,
            });
        }
    }

    // Generate trait with #[async_trait] and async methods
    let trait_attrs = &input_trait.attrs;
    let trait_supertraits = &input_trait.supertraits;
    let trait_methods: Vec<_> = methods.iter().map(|m| {
        let name = &m.method_name;
        let ok_type = &m.ok_type;
        let params: Vec<_> = m.param_names.iter().zip(m.param_types.iter()).map(|(n, t)| {
            quote! { #n: #t }
        }).collect();
        quote! {
            async fn #name(&self, #(#params),*) -> Result<#ok_type, String>;
        }
    }).collect();

    let trait_output = quote! {
        #(#trait_attrs)*
        #[::async_trait::async_trait]
        #trait_vis trait #trait_name: #trait_supertraits {
            #(#trait_methods)*
        }
    };

    // Generate Proxy
    let proxy_methods: Vec<_> = methods.iter().map(|m| {
        let name = &m.method_name;
        let full_name = &m.full_method_name;
        let ok_type = &m.ok_type;
        let param_names = &m.param_names;
        let param_types = &m.param_types;

        let params: Vec<_> = param_names.iter().zip(param_types.iter()).map(|(n, t)| {
            quote! { #n: #t }
        }).collect();

        let serialize_params = if param_names.is_empty() {
            quote! { serde_json::Value::Null }
        } else if param_names.len() == 1 {
            let p = &param_names[0];
            quote! { serde_json::to_value(&#p).map_err(|e| e.to_string())? }
        } else {
            quote! { serde_json::to_value(&(#(&#param_names),*)).map_err(|e| e.to_string())? }
        };

        quote! {
            async fn #name(&self, #(#params),*) -> Result<#ok_type, String> {
                let params = #serialize_params;
                let result = self.__endpoint.call(#full_name, params).await?;
                serde_json::from_value(result).map_err(|e| e.to_string())
            }
        }
    }).collect();

    let proxy_output = quote! {
        #trait_vis struct #proxy_name {
            __endpoint: std::sync::Arc<mad_hatter::tunnel::TunnelEndpoint>,
        }

        impl #proxy_name {
            pub fn new(endpoint: std::sync::Arc<mad_hatter::tunnel::TunnelEndpoint>) -> Self {
                Self { __endpoint: endpoint }
            }
        }

        #[::async_trait::async_trait]
        impl #trait_name for #proxy_name {
            #(#proxy_methods)*
        }
    };

    // Generate Dispatcher
    let dispatch_arms: Vec<_> = methods.iter().map(|m| {
        let name = &m.method_name;
        let full_name = &m.full_method_name;
        let param_names = &m.param_names;
        let param_types = &m.param_types;

        let deserialize_params = if param_names.is_empty() {
            quote! {}
        } else if param_names.len() == 1 {
            let p = &param_names[0];
            let t = &param_types[0];
            quote! {
                let #p: #t = serde_json::from_value(params).map_err(|e| e.to_string())?;
            }
        } else {
            let indices: Vec<_> = (0..param_names.len()).map(syn::Index::from).collect();
            quote! {
                let __tuple: (#(#param_types),*) = serde_json::from_value(params).map_err(|e| e.to_string())?;
                #(let #param_names = __tuple.#indices;)*
            }
        };

        quote! {
            #full_name => {
                #deserialize_params
                let result = self.__handler.#name(#(#param_names),*).await;
                match result {
                    Ok(v) => Ok(serde_json::to_value(&v).map_err(|e| e.to_string())?),
                    Err(e) => Err(e),
                }
            }
        }
    }).collect();

    let dispatcher_output = quote! {
        #trait_vis struct #dispatcher_name<T: #trait_name + 'static> {
            __handler: std::sync::Arc<T>,
        }

        impl<T: #trait_name + 'static> #dispatcher_name<T> {
            pub fn new(handler: std::sync::Arc<T>) -> Self {
                Self { __handler: handler }
            }

            pub fn boxed(handler: std::sync::Arc<T>) -> Box<dyn mad_hatter::tunnel::Dispatch> {
                Box::new(Self::new(handler))
            }
        }

        #[::async_trait::async_trait]
        impl<T: #trait_name + 'static> mad_hatter::tunnel::Dispatch for #dispatcher_name<T> {
            fn service_name(&self) -> &str {
                #service_name_str
            }

            async fn dispatch(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
                match method {
                    #(#dispatch_arms)*
                    _ => Err(format!("unknown method: {}", method)),
                }
            }
        }
    };

    let output = quote! {
        #trait_output
        #proxy_output
        #dispatcher_output
    };

    output.into()
}

struct MethodInfo<'a> {
    method_name: syn::Ident,
    full_method_name: String,
    ok_type: Box<Type>,
    param_names: Vec<&'a syn::Pat>,
    param_types: Vec<&'a Type>,
}

fn extract_result_ok_type(return_type: &ReturnType) -> Box<Type> {
    if let ReturnType::Type(_, ty) = return_type {
        if let Type::Path(type_path) = ty.as_ref() {
            if let Some(segment) = type_path.path.segments.last() {
                if segment.ident == "Result" {
                    if let PathArguments::AngleBracketed(args) = &segment.arguments {
                        if let Some(GenericArgument::Type(ok_type)) = args.args.first() {
                            return Box::new(ok_type.clone());
                        }
                    }
                }
            }
        }
    }
    panic!("tunnel_service methods must return Result<T, String>");
}