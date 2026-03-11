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

// ============================================================
// FromMarkdown derive for enums
// ============================================================

/// Information about a single enum variant, extracted at macro time.
struct VariantInfo {
    ident: syn::Ident,
    snake_name: String,
    doc: String,
    fields: Vec<FieldInfo>,
}

/// Information about a single field within a variant.
struct FieldInfo {
    name: String,
    doc: String,
    is_option: bool,
    /// The inner type name for Option fields (e.g. "u64", "String")
    inner_type: String,
}

/// Generate `impl FromMarkdown for Enum`.
pub fn derive_from_markdown(input: DeriveInput) -> TokenStream {
    let enum_name = &input.ident;
    let enum_name_str = enum_name.to_string();

    let variants_data = match &input.data {
        Data::Enum(data) => &data.variants,
        _ => {
            return syn::Error::new_spanned(&input.ident, "FromMarkdown only supports enums")
                .to_compile_error();
        }
    };

    // Extract variant info
    let mut variants: Vec<VariantInfo> = Vec::new();
    for v in variants_data {
        let docs = extract_doc_comments(&v.attrs);
        let doc = docs.join(" ");
        let snake_name = to_snake_case(&v.ident.to_string());

        let fields = match &v.fields {
            Fields::Named(named) => {
                named.named.iter().map(|f| {
                    let fname = f.ident.as_ref().unwrap().to_string();
                    let fdocs = extract_doc_comments(&f.attrs);
                    let fdoc = if fdocs.is_empty() { fname.clone() } else { fdocs.join(" ") };
                    let (is_option, inner_type) = check_option_type(&f.ty);
                    FieldInfo { name: fname, doc: fdoc, is_option, inner_type }
                }).collect()
            }
            Fields::Unit => Vec::new(),
            _ => {
                return syn::Error::new_spanned(&v.ident, "FromMarkdown only supports unit or named-field variants")
                    .to_compile_error();
            }
        };

        variants.push(VariantInfo {
            ident: v.ident.clone(),
            snake_name,
            doc,
            fields,
        });
    }

    let schema_body = gen_schema_markdown(&enum_name_str, &variants);
    let parse_body = gen_from_markdown(enum_name, &enum_name_str, &variants);

    quote! {
        impl ::mad_hatter::llm::FromMarkdown for #enum_name {
            fn schema_markdown(token: &str) -> ::std::string::String {
                #schema_body
            }
            fn from_markdown(text: &str, token: &str) -> ::std::result::Result<::std::vec::Vec<Self>, ::std::string::String> {
                #parse_body
            }
        }
    }
}

/// Generate the body of `schema_markdown`.
///
/// New format: `{TypeName}-{token}` as element separator, first line is variant name.
///
/// 0 fields:
///   action {doc}
///   首行输出: {TypeName}-{token}
///   第二行输出: {variant_name}
///
/// 1 field (non-option):
///   action {doc}
///   首行输出: {TypeName}-{token}
///   第二行输出: {variant_name}
///   第三行开始多行输出{field_doc}
///
/// 0 required + 1 option field (like Idle):
///   action {doc}
///   首行输出: {TypeName}-{token}
///   第二行输出: {variant_name}
///   第三行可选输出: {field_doc}
///
/// N>=2 fields:
///   action {doc}
///   首行输出: {TypeName}-{token}
///   第二行输出: {variant_name}
///   {field_name}-{token}
///   {field_doc}
///   ...
fn gen_schema_markdown(enum_name_str: &str, variants: &[VariantInfo]) -> TokenStream {
    let mut variant_schemas = Vec::new();

    for vi in variants {
        let snake = &vi.snake_name;
        let doc = &vi.doc;
        let required_fields: Vec<&FieldInfo> = vi.fields.iter().filter(|f| !f.is_option).collect();
        let option_fields: Vec<&FieldInfo> = vi.fields.iter().filter(|f| f.is_option).collect();
        let total = vi.fields.len();

        if total == 0 {
            // 0 fields: just variant name
            variant_schemas.push(quote! {
                __out.push_str(&::std::format!(
                    "action {doc}\n首行输出: {ename}-{token}\n第二行输出: {snake}\n",
                    doc = #doc, ename = #enum_name_str, token = token, snake = #snake
                ));
            });
        } else if total == 1 && option_fields.len() == 1 {
            // 0 required + 1 option (like Idle)
            let fdoc = &option_fields[0].doc;
            variant_schemas.push(quote! {
                __out.push_str(&::std::format!(
                    "action {doc}\n首行输出: {ename}-{token}\n第二行输出: {snake}\n第三行可选输出: {fdoc}\n",
                    doc = #doc, ename = #enum_name_str, token = token, snake = #snake, fdoc = #fdoc
                ));
            });
        } else if total == 1 && required_fields.len() == 1 {
            // 1 required field
            let fdoc = &required_fields[0].doc;
            variant_schemas.push(quote! {
                __out.push_str(&::std::format!(
                    "action {doc}\n首行输出: {ename}-{token}\n第二行输出: {snake}\n第三行开始多行输出{fdoc}\n",
                    doc = #doc, ename = #enum_name_str, token = token, snake = #snake, fdoc = #fdoc
                ));
            });
        } else {
            // N>=2 fields: use field separators
            let mut field_lines = Vec::new();
            for f in &vi.fields {
                let fname = &f.name;
                let fdoc = &f.doc;
                if f.is_option {
                    field_lines.push(quote! {
                        __out.push_str(&::std::format!(
                            "{fname}-{token}\n{fdoc}（可选）\n",
                            fname = #fname, token = token, fdoc = #fdoc
                        ));
                    });
                } else {
                    field_lines.push(quote! {
                        __out.push_str(&::std::format!(
                            "{fname}-{token}\n{fdoc}\n",
                            fname = #fname, token = token, fdoc = #fdoc
                        ));
                    });
                }
            }
            variant_schemas.push(quote! {
                __out.push_str(&::std::format!(
                    "action {doc}\n首行输出: {ename}-{token}\n第二行输出: {snake}\n",
                    doc = #doc, ename = #enum_name_str, token = token, snake = #snake
                ));
                #(#field_lines)*
            });
        }

        // Add blank line between variants
        variant_schemas.push(quote! {
            __out.push('\n');
        });
    }

    quote! {
        let mut __out = ::std::string::String::new();
        #(#variant_schemas)*
        __out.push_str(&::std::format!("最后输出: {ename}-end-{token}\n", ename = #enum_name_str, token = token));
        __out
    }
}

/// Generate the body of `from_markdown`.
///
/// Split by `{TypeName}-{token}`, then parse each block.
fn gen_from_markdown(enum_name: &syn::Ident, enum_name_str: &str, variants: &[VariantInfo]) -> TokenStream {
    // Build match arms for each variant
    let mut match_arms = Vec::new();

    for vi in variants {
        let snake = &vi.snake_name;
        let variant_ident = &vi.ident;
        let total = vi.fields.len();
        let required_fields: Vec<&FieldInfo> = vi.fields.iter().filter(|f| !f.is_option).collect();
        let option_fields: Vec<&FieldInfo> = vi.fields.iter().filter(|f| f.is_option).collect();

        if total == 0 {
            // 0 fields
            match_arms.push(quote! {
                #snake => {
                    __results.push(#enum_name::#variant_ident);
                }
            });
        } else if total == 1 && option_fields.len() == 1 {
            // 0 required + 1 option field (like Idle { timeout_secs: Option<u64> })
            let field_name_ident = syn::Ident::new(&option_fields[0].name, proc_macro2::Span::call_site());
            let inner = &option_fields[0].inner_type;

            if inner == "u64" || inner == "i64" || inner == "u32" || inner == "i32" || inner == "usize" {
                match_arms.push(quote! {
                    #snake => {
                        let __rest = __body.trim();
                        let __val = if __rest.is_empty() {
                            ::std::option::Option::None
                        } else {
                            match __rest.parse() {
                                Ok(v) => ::std::option::Option::Some(v),
                                Err(e) => return ::std::result::Result::Err(
                                    ::std::format!("Failed to parse field '{}' of variant '{}': {}", #snake, stringify!(#variant_ident), e)
                                ),
                            }
                        };
                        __results.push(#enum_name::#variant_ident { #field_name_ident: __val });
                    }
                });
            } else {
                // Option<String>
                match_arms.push(quote! {
                    #snake => {
                        let __rest = __body.trim_end();
                        let __val = if __rest.is_empty() {
                            ::std::option::Option::None
                        } else {
                            ::std::option::Option::Some(__rest.to_string())
                        };
                        __results.push(#enum_name::#variant_ident { #field_name_ident: __val });
                    }
                });
            }
        } else if total == 1 && required_fields.len() == 1 {
            // 1 required field (like Thinking { content: String })
            let field_name_ident = syn::Ident::new(&required_fields[0].name, proc_macro2::Span::call_site());
            match_arms.push(quote! {
                #snake => {
                    let __val = __body.trim_end().to_string();
                    __results.push(#enum_name::#variant_ident { #field_name_ident: __val });
                }
            });
        } else {
            // N>=2 fields
            let multi_parse = gen_multi_field_parse(enum_name, vi);
            match_arms.push(quote! {
                #snake => {
                    #multi_parse
                }
            });
        }
    }

    quote! {
        let mut __results: ::std::vec::Vec<#enum_name> = ::std::vec::Vec::new();
        let __separator = ::std::format!("{}-{}", #enum_name_str, token);
        let __end_marker = ::std::format!("{}-end-{}", #enum_name_str, token);

        // Check for end marker (truncation defense)
        let __trimmed = text.trim_end();
        if !__trimmed.ends_with(&__end_marker) {
            return ::std::result::Result::Err(
                ::std::format!("Output truncated: missing end marker {}", __end_marker)
            );
        }

        // Strip end marker before parsing
        let __text_without_end = &__trimmed[..__trimmed.len() - __end_marker.len()];

        // Split by separator line
        let __blocks: ::std::vec::Vec<&str> = __text_without_end.split(&__separator).collect();

        // Skip first block (content before first separator, usually empty or preamble)
        for __block in __blocks.iter().skip(1) {
            // Trim leading newline
            let __block = if __block.starts_with('\n') { &__block[1..] } else { *__block };

            if __block.trim().is_empty() {
                continue;
            }

            // First line is variant name
            let (__variant_line, __body) = match __block.find('\n') {
                Some(pos) => (__block[..pos].trim(), &__block[pos + 1..]),
                None => (__block.trim(), ""),
            };

            match __variant_line {
                #(#match_arms)*
                __unknown => {
                    return ::std::result::Result::Err(
                        ::std::format!("Unknown variant: '{}'", __unknown)
                    );
                }
            }
        }

        ::std::result::Result::Ok(__results)
    }
}

/// Generate parsing code for a variant with N>=2 fields.
///
/// Uses `{field_name}-{token}` separators to split the body.
fn gen_multi_field_parse(enum_name: &syn::Ident, vi: &VariantInfo) -> TokenStream {
    let variant_ident = &vi.ident;
    let snake = &vi.snake_name;

    // Build separator names and field processing
    let mut field_names: Vec<String> = Vec::new();
    let mut field_idents: Vec<syn::Ident> = Vec::new();
    let mut field_is_option: Vec<bool> = Vec::new();
    let mut field_inner_types: Vec<String> = Vec::new();

    for f in &vi.fields {
        field_names.push(f.name.clone());
        field_idents.push(syn::Ident::new(&f.name, proc_macro2::Span::call_site()));
        field_is_option.push(f.is_option);
        field_inner_types.push(f.inner_type.clone());
    }

    let field_count = field_names.len();

    // Generate separator strings at runtime
    let sep_name_literals: Vec<&str> = field_names.iter().map(|s| s.as_str()).collect();

    // Build field extraction code
    let mut field_extractions = Vec::new();
    let mut field_var_names: Vec<syn::Ident> = Vec::new();

    for (i, f) in vi.fields.iter().enumerate() {
        let var_name = syn::Ident::new(&format!("__field_{}", f.name), proc_macro2::Span::call_site());
        let fname = &f.name;

        if f.is_option {
            let inner = &f.inner_type;
            if inner == "u64" || inner == "i64" || inner == "u32" || inner == "i32" || inner == "usize" {
                field_extractions.push(quote! {
                    let #var_name = {
                        let __raw = __field_values.get(#i).map(|s| s.trim()).unwrap_or("");
                        if __raw.is_empty() {
                            ::std::option::Option::None
                        } else {
                            match __raw.parse() {
                                Ok(v) => ::std::option::Option::Some(v),
                                Err(e) => return ::std::result::Result::Err(
                                    ::std::format!("Failed to parse field '{}' of variant '{}': {}", #fname, #snake, e)
                                ),
                            }
                        }
                    };
                });
            } else {
                field_extractions.push(quote! {
                    let #var_name = {
                        let __raw = __field_values.get(#i).map(|s| s.trim_end()).unwrap_or("");
                        if __raw.is_empty() {
                            ::std::option::Option::None
                        } else {
                            ::std::option::Option::Some(__raw.to_string())
                        }
                    };
                });
            }
        } else {
            field_extractions.push(quote! {
                let #var_name = {
                    let __raw = __field_values.get(#i).map(|s| *s).unwrap_or("");
                    if #i == #field_count - 1 {
                        __raw.trim_end().to_string()
                    } else {
                        __raw.trim_end_matches('\n').to_string()
                    }
                };
            });
        }

        field_var_names.push(var_name);
    }

    // Build constructor
    let constructor_fields: Vec<TokenStream> = field_idents.iter().zip(field_var_names.iter()).map(|(fi, vn)| {
        quote! { #fi: #vn }
    }).collect();

    quote! {
        // Build separators: ["{field_name}-{token}", ...]
        let __seps: ::std::vec::Vec<::std::string::String> = [#(#sep_name_literals),*]
            .iter()
            .map(|n| ::std::format!("{}-{}", n, token))
            .collect();

        // Split body by field separators sequentially
        let mut __field_values: ::std::vec::Vec<&str> = ::std::vec::Vec::new();
        let mut __remaining = __body;

        for (i, sep) in __seps.iter().enumerate() {
            // Find separator line
            let __sep_with_newline = ::std::format!("{}\n", sep);
            match __remaining.find(&__sep_with_newline) {
                Some(pos) => {
                    // Content before this separator belongs to previous field (if any)
                    // Skip to after separator
                    __remaining = &__remaining[pos + __sep_with_newline.len()..];
                }
                None => {
                    // Check if separator is at end without trailing newline
                    if __remaining.trim_end() == sep.as_str() {
                        __remaining = "";
                    } else {
                        // For optional fields, separator might not be present
                        // Push empty and continue
                        __field_values.push("");
                        continue;
                    }
                }
            }

            // Find next separator to delimit this field's value
            let mut __end = __remaining.len();
            for next_sep in __seps.iter().skip(i + 1) {
                let __next_sep_with_newline = ::std::format!("{}\n", next_sep);
                if let Some(pos) = __remaining.find(&__next_sep_with_newline) {
                    __end = pos;
                    break;
                }
                // Also check bare separator at end
                let __trimmed = __remaining.trim_end();
                if __trimmed.ends_with(next_sep.as_str()) {
                    __end = __trimmed.len() - next_sep.len();
                    break;
                }
            }

            __field_values.push(&__remaining[..__end]);
            __remaining = &__remaining[__end..];
        }

        #(#field_extractions)*

        __results.push(#enum_name::#variant_ident {
            #(#constructor_fields),*
        });
    }
}

// ============================================================
// Shared helpers
// ============================================================

/// Convert CamelCase to snake_case.
fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(c.to_lowercase().next().unwrap());
        } else {
            result.push(c);
        }
    }
    result
}

/// Check if a type is `Option<T>` and return (is_option, inner_type_name).
fn check_option_type(ty: &syn::Type) -> (bool, String) {
    if let syn::Type::Path(type_path) = ty {
        let last_seg = type_path.path.segments.last();
        if let Some(seg) = last_seg {
            if seg.ident == "Option" {
                if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                    if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                        // Get the inner type as string
                        let inner_str = quote!(#inner_ty).to_string().replace(' ', "");
                        return (true, inner_str);
                    }
                }
            }
        }
    }
    (false, String::new())
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
