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

/// Bind a user-defined trait to HTTP routes.
///
/// Unlike `http_service!`, this macro does **not** generate a trait.
/// The trait is hand-written by the user as a pure business interface
/// with zero framework dependencies. `bind_http!` generates axum
/// handlers and a router that delegate to the trait implementation.
///
/// ```ignore
/// // User-written trait (no Mad Hatter dependency)
/// trait UserService {
///     async fn get_user(&self, id: u64) -> User;
///     async fn create_user(&self, body: CreateUserReq) -> User;
///     async fn delete_user(&self, id: u64);
/// }
///
/// impl UserService for App { ... }
///
/// // Bind trait methods to HTTP routes
/// bind_http! {
///     UserService for App {
///         get_user(id: u64)              => GET    "users/{id}" -> Json<User>;
///         create_user(body: CreateUserReq) => POST   "users"     -> Json<User>;
///         delete_user(id: u64)            => DELETE "users/{id}";
///     }
/// }
/// ```
#[proc_macro]
pub fn bind_http(input: TokenStream) -> TokenStream {
    let definition = syn::parse_macro_input!(input as parse::BindDef);
    gen::generate_bind(definition).into()
}
