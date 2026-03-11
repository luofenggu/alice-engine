use proc_macro::TokenStream;

mod parse;
mod gen;
mod llm;

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

/// Derive `ToMarkdown` for a struct.
///
/// Renders struct fields as markdown sections.
///
/// ```ignore
/// #[derive(ToMarkdown)]
/// /// You are a knowledge expert.
/// struct CaptureInput {
///     #[markdown(skip)]
///     end_marker: String,
///     /// Current knowledge
///     knowledge: String,
/// }
///
/// let input = CaptureInput { end_marker: "X".into(), knowledge: "...".into() };
/// let md = input.to_markdown();
/// // "You are a knowledge expert.\n\n### Current knowledge ###\n...\n"
/// ```
#[proc_macro_derive(ToMarkdown, attributes(markdown))]
pub fn derive_to_markdown(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    llm::derive_to_markdown(input).into()
}

/// Derive `FromMarkdown` for an enum.
///
/// Generates both format schema (for LLM prompts) and parser (for LLM output).
///
/// ```ignore
/// #[derive(FromMarkdown)]
/// enum Action {
///     /// 阅读收件箱
///     ReadMsg,
///     /// 记录思考
///     Thinking {
///         /// 思考内容
///         content: String,
///     },
///     /// 寄出信件
///     SendMsg {
///         /// 收件人
///         recipient: String,
///         /// 信件内容
///         content: String,
///     },
/// }
/// ```
#[proc_macro_derive(FromMarkdown, attributes(markdown))]
pub fn derive_from_markdown(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    llm::derive_from_markdown(input).into()
}
