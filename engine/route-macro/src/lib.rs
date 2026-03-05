use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn, LitStr};

fn route_impl(method: &str, attr: TokenStream, item: TokenStream) -> TokenStream {
    let path = parse_macro_input!(attr as LitStr);
    let input = parse_macro_input!(item as ItemFn);
    let fn_name = &input.sig.ident;
    let const_name = syn::Ident::new(
        &format!("ROUTE_{}", fn_name.to_string().to_uppercase()),
        fn_name.span(),
    );
    let method_const_name = syn::Ident::new(
        &format!("METHOD_{}", fn_name.to_string().to_uppercase()),
        fn_name.span(),
    );
    let method_lit = syn::LitStr::new(method, fn_name.span());
    let path_value = path.value();
    let path_lit = syn::LitStr::new(&path_value, path.span());

    let output = quote! {
        #input

        #[allow(dead_code)]
        pub const #const_name: &str = #path_lit;

        #[allow(dead_code)]
        pub const #method_const_name: &str = #method_lit;
    };

    output.into()
}

#[proc_macro_attribute]
pub fn get(attr: TokenStream, item: TokenStream) -> TokenStream {
    route_impl("GET", attr, item)
}

#[proc_macro_attribute]
pub fn post(attr: TokenStream, item: TokenStream) -> TokenStream {
    route_impl("POST", attr, item)
}

#[proc_macro_attribute]
pub fn put(attr: TokenStream, item: TokenStream) -> TokenStream {
    route_impl("PUT", attr, item)
}

#[proc_macro_attribute]
pub fn delete(attr: TokenStream, item: TokenStream) -> TokenStream {
    route_impl("DELETE", attr, item)
}

#[proc_macro_attribute]
pub fn patch(attr: TokenStream, item: TokenStream) -> TokenStream {
    route_impl("PATCH", attr, item)
}

#[proc_macro_attribute]
pub fn any_method(attr: TokenStream, item: TokenStream) -> TokenStream {
    route_impl("ANY", attr, item)
}

