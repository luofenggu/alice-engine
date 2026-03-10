use syn::{
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    Ident, LitStr, Token, Type, Result,
};

/// Top-level: `service Name { ... }`
pub struct ServiceDef {
    pub name: Ident,
    pub endpoints: Vec<EndpointDef>,
}

/// HTTP method
#[derive(Debug, Clone)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
}

/// A single endpoint: `METHOD "path" => fn_name(params) -> RetType;`
pub struct EndpointDef {
    pub method: HttpMethod,
    pub path: String,
    pub fn_name: Ident,
    pub params: Vec<ParamDef>,
    pub return_type: Option<Type>,
}

/// A parameter: `name: Type`
pub struct ParamDef {
    pub name: Ident,
    pub ty: Type,
}

/// Top-level bind definition: `TraitName for ImplType { ... }`
pub struct BindDef {
    pub trait_name: Ident,
    pub impl_type: Type,
    pub bindings: Vec<BindingDef>,
}

/// A single binding: `fn_name(params) => METHOD "path" -> RetType;`
pub struct BindingDef {
    pub fn_name: Ident,
    pub params: Vec<ParamDef>,
    pub method: HttpMethod,
    pub path: String,
    pub return_type: Option<Type>,
}

impl Parse for ServiceDef {
    fn parse(input: ParseStream) -> Result<Self> {
        // Parse `service`
        let service_kw: Ident = input.parse()?;
        if service_kw != "service" {
            return Err(syn::Error::new(service_kw.span(), "expected `service`"));
        }

        let name: Ident = input.parse()?;

        let content;
        syn::braced!(content in input);

        let mut endpoints = Vec::new();
        while !content.is_empty() {
            endpoints.push(content.parse::<EndpointDef>()?);
        }

        Ok(ServiceDef { name, endpoints })
    }
}

impl Parse for EndpointDef {
    fn parse(input: ParseStream) -> Result<Self> {
        // Parse HTTP method (GET, POST, PUT, DELETE, PATCH)
        let method_ident: Ident = input.parse()?;
        let method = parse_http_method(&method_ident)?;

        // Parse path string
        let path_lit: LitStr = input.parse()?;
        let path = path_lit.value();

        // Parse `=>`
        input.parse::<Token![=>]>()?;

        // Parse function name
        let fn_name: Ident = input.parse()?;

        // Parse parameters in parentheses
        let params_content;
        syn::parenthesized!(params_content in input);
        let params_punctuated: Punctuated<ParamDef, Token![,]> =
            Punctuated::parse_terminated(&params_content)?;
        let params: Vec<ParamDef> = params_punctuated.into_iter().collect();

        // Parse optional return type: `-> Type`
        let return_type = if input.peek(Token![->]) {
            input.parse::<Token![->]>()?;
            Some(input.parse::<Type>()?)
        } else {
            None
        };

        // Parse trailing semicolon
        input.parse::<Token![;]>()?;

        Ok(EndpointDef {
            method,
            path,
            fn_name,
            params,
            return_type,
        })
    }
}

impl Parse for ParamDef {
    fn parse(input: ParseStream) -> Result<Self> {
        let name: Ident = input.parse()?;
        input.parse::<Token![:]>()?;
        let ty: Type = input.parse()?;
        Ok(ParamDef { name, ty })
    }
}

impl Parse for BindDef {
    fn parse(input: ParseStream) -> Result<Self> {
        // Parse `TraitName`
        let trait_name: Ident = input.parse()?;

        // Parse `for` keyword
        input.parse::<Token![for]>()?;

        // Parse `ImplType`
        let impl_type: Type = input.parse()?;

        // Parse `{ ... }`
        let content;
        syn::braced!(content in input);

        let mut bindings = Vec::new();
        while !content.is_empty() {
            bindings.push(content.parse::<BindingDef>()?);
        }

        Ok(BindDef {
            trait_name,
            impl_type,
            bindings,
        })
    }
}

impl Parse for BindingDef {
    fn parse(input: ParseStream) -> Result<Self> {
        // Parse function name
        let fn_name: Ident = input.parse()?;

        // Parse parameters in parentheses
        let params_content;
        syn::parenthesized!(params_content in input);
        let params_punctuated: Punctuated<ParamDef, Token![,]> =
            Punctuated::parse_terminated(&params_content)?;
        let params: Vec<ParamDef> = params_punctuated.into_iter().collect();

        // Parse `=>`
        input.parse::<Token![=>]>()?;

        // Parse HTTP method
        let method_ident: Ident = input.parse()?;
        let method = parse_http_method(&method_ident)?;

        // Parse path string
        let path_lit: LitStr = input.parse()?;
        let path = path_lit.value();

        // Parse optional return type: `-> Type`
        let return_type = if input.peek(Token![->]) {
            input.parse::<Token![->]>()?;
            Some(input.parse::<Type>()?)
        } else {
            None
        };

        // Parse trailing semicolon
        input.parse::<Token![;]>()?;

        Ok(BindingDef {
            fn_name,
            params,
            method,
            path,
            return_type,
        })
    }
}

fn parse_http_method(ident: &Ident) -> Result<HttpMethod> {
    match ident.to_string().as_str() {
        "GET" => Ok(HttpMethod::Get),
        "POST" => Ok(HttpMethod::Post),
        "PUT" => Ok(HttpMethod::Put),
        "DELETE" => Ok(HttpMethod::Delete),
        "PATCH" => Ok(HttpMethod::Patch),
        other => Err(syn::Error::new(
            ident.span(),
            format!("unknown HTTP method: {}", other),
        )),
    }
}