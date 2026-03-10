use proc_macro::TokenStream;

mod parse;
mod gen;

/// Define an HTTP service with typed endpoints.
///
/// ```ignore
/// http_service! {
///     service Demo {
///         GET    "users/{id}"  => get_user(id: u64) -> Json<User>;
///         POST   "users"       => create_user(body: CreateUserReq) -> Json<User>;
///         DELETE "users/{id}"  => delete_user(id: u64);
///     }
/// }
/// ```
#[proc_macro]
pub fn http_service(input: TokenStream) -> TokenStream {
    let definition = syn::parse_macro_input!(input as parse::ServiceDef);
    gen::generate(definition).into()
}
