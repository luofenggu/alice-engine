use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, Lit, Meta};

/// Generate `impl ToMarkdown for Struct` from derive attributes.
pub fn derive_to_markdown(input: DeriveInput) -> TokenStream {
    let struct_name = &input.ident;

    // Extract struct-level doc comments
    let struct_docs = extract_doc_comments(&input.attrs);

    // Ensure it's a struct with named fields
    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return syn::Error::new_spanned(&input.ident, "ToMarkdown only supports structs with named fields")
                    .to_compile_error();
            }
        },
        _ => {
            return syn::Error::new_spanned(&input.ident, "ToMarkdown only supports structs")
                .to_compile_error();
        }
    };

    // Process each field
    let mut field_renders = Vec::new();

    for field in fields {
        let field_name = field.ident.as_ref().unwrap();

        // Check for #[markdown(skip)]
        if has_markdown_skip(&field.attrs) {
            continue;
        }

        // Extract field-level doc comments
        let field_docs = extract_doc_comments(&field.attrs);

        // Determine section title: doc comment if present, otherwise field name
        let section_title = if field_docs.is_empty() {
            field_name.to_string()
        } else {
            field_docs.join("\n")
        };

        // Generate render code for this field
        // P0: only String fields supported
        field_renders.push(quote! {
            {
                let value = &self.#field_name;
                let s: &str = value.as_ref();
                if !s.is_empty() {
                    if !__out.is_empty() && !__out.ends_with('\n') {
                        __out.push('\n');
                    }
                    __out.push_str("\n### ");
                    __out.push_str(#section_title);
                    __out.push_str(" ###\n");
                    __out.push_str(s);
                    __out.push('\n');
                }
            }
        });
    }

    // Build the header from struct-level doc comments
    let header = if struct_docs.is_empty() {
        quote! {}
    } else {
        let header_text = struct_docs.join("\n");
        quote! {
            __out.push_str(#header_text);
            __out.push('\n');
        }
    };

    quote! {
        impl ::mad_hatter::llm::ToMarkdown for #struct_name {
            fn to_markdown(&self) -> ::std::string::String {
                let mut __out = ::std::string::String::new();
                #header
                #(#field_renders)*
                __out
            }
        }
    }
}

/// Extract doc comments from attributes.
/// `#[doc = " some text"]` → `"some text"` (trimmed)
fn extract_doc_comments(attrs: &[syn::Attribute]) -> Vec<String> {
    let mut docs = Vec::new();
    for attr in attrs {
        if attr.path().is_ident("doc") {
            if let Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(expr_lit) = &nv.value {
                    if let Lit::Str(s) = &expr_lit.lit {
                        docs.push(s.value().trim().to_string());
                    }
                }
            }
        }
    }
    docs
}

/// Check if a field has `#[markdown(skip)]` attribute.
fn has_markdown_skip(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("markdown") {
            // Parse as list: markdown(skip)
            if let Ok(nested) = attr.parse_args::<syn::Ident>() {
                if nested == "skip" {
                    return true;
                }
            }
        }
    }
    false
}

