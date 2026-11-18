#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse_macro_input, Data, DataStruct, DeriveInput, Fields, Lit, Path};

#[derive(Debug)]
struct FieldMeta {
    desc: Option<String>,
    min: Option<f64>,
    max: Option<f64>,
    /// Exact decimal spellings of the `min`/`max` attribute literals. The
    /// `f64` bounds above cannot name integer edges exactly (`u64::MAX`,
    /// `i64::MIN`); the raws preserve them for schema consumers.
    min_raw: Option<String>,
    max_raw: Option<String>,
    clamp: bool,
    skip: bool,
    required: bool,
    format: Option<String>,
    max_len: Option<usize>,
    sanitize: Option<String>,
    pattern: Option<String>,
    coerce: Option<String>,
    parse: Option<String>,
    empty_is_none: bool,
    variants: Option<TokenStream2>,
    tab: Option<String>,
    group: Option<String>,
    tooltip: Option<String>,
    step: Option<f64>,
    inline_group: Option<String>,
    /// Which axis this field measures: `correctness`, `task-value`, or
    /// `producer-confidence`. A gate that cannot name its axis is a gate whose
    /// meaning is ambiguous, so the vocabulary is closed at parse time.
    axis: Option<String>,
    /// The member belongs to one arm of a tagged union; editable only while
    /// the selector has the named value (`"kind=breakout"`).
    visible_when: Option<String>,
    /// Maximum item count for a list member (`#[field(items = N)]`).
    items: Option<usize>,
    /// The list is fixed-arity (`[T; N]`) rather than open (`Vec<T>`).
    fixed: bool,
    /// The field embeds another component at a dotted prefix.
    nested: bool,
}

/// Parse a `min`/`max` bound literal: an unsigned int/float, or a
/// unary-negated one (`-5`, `i64::MIN`). Returns the legacy `f64` value for
/// the runtime bound check and the exact decimal spelling for the raw schema
/// trace. Anything else is a contract error, not a silent drop: a bound
/// that does not bind must fail the build.
///
/// The sign is restored from the token spelling: digit accessors may or may
/// not carry it depending on the literal form, but the spelling never lies —
/// without this, `min = -5` would silently bind `5`.
fn bound_from_lit(lit: &Lit) -> Result<(f64, String), String> {
    let (digits, token) = match lit {
        Lit::Int(i) => (i.base10_digits().to_string(), i.to_string()),
        Lit::Float(f) => (f.base10_digits().to_string(), f.to_string()),
        _ => return Err("bound must be an integer or float literal".to_string()),
    };
    let unsigned = digits.strip_prefix('-').unwrap_or(&digits).to_string();
    let raw = if token.starts_with('-') {
        format!("-{unsigned}")
    } else {
        unsigned
    };
    raw.parse::<f64>()
        .map(|num| (num, raw.clone()))
        .map_err(|_| format!("bound `{raw}` is not a number"))
}

fn parse_field_attrs(field: &syn::Field) -> Result<FieldMeta, syn::Error> {
    let mut result = FieldMeta {
        desc: None,
        min: None,
        max: None,
        min_raw: None,
        max_raw: None,
        clamp: false,
        skip: false,
        required: true,
        format: None,
        max_len: None,
        sanitize: None,
        pattern: None,
        coerce: None,
        parse: None,
        empty_is_none: true,
        variants: None,
        tab: None,
        group: None,
        tooltip: None,
        step: None,
        inline_group: None,
        axis: None,
        visible_when: None,
        items: None,
        fixed: false,
        nested: false,
    };

    for attr in &field.attrs {
        if !attr.path().is_ident("field") {
            continue;
        }

        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("skip") {
                result.skip = true;
                return Ok(());
            }
            if meta.path.is_ident("desc") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(s) = lit {
                    result.desc = Some(s.value());
                }
                return Ok(());
            }
            if meta.path.is_ident("min") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                let (num, raw) =
                    bound_from_lit(&lit).map_err(|message| meta.error(message))?;
                result.min = Some(num);
                result.min_raw = Some(raw);
                return Ok(());
            }
            if meta.path.is_ident("max") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                let (num, raw) =
                    bound_from_lit(&lit).map_err(|message| meta.error(message))?;
                result.max = Some(num);
                result.max_raw = Some(raw);
                return Ok(());
            }
            if meta.path.is_ident("required") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Bool(b) = lit {
                    result.required = b.value();
                }
                return Ok(());
            }
            if meta.path.is_ident("format") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(s) = lit {
                    result.format = Some(s.value());
                }
                return Ok(());
            }
            if meta.path.is_ident("max_len") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Int(i) = lit {
                    let max_len = i.base10_parse::<usize>().map_err(|error| {
                        meta.error(format!("`max_len` must fit in a usize: {error}"))
                    })?;
                    result.max_len = Some(max_len);
                }
                return Ok(());
            }
            if meta.path.is_ident("sanitize") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(s) = lit {
                    let mode = s.value();
                    match mode.as_str() {
                        "trim" | "lowercase" | "strip_html" | "slugify" => {
                            result.sanitize = Some(mode);
                        }
                        _ => {
                            return Err(meta.error(format!(
                                "unknown sanitize mode `{mode}`, expected `trim`, `lowercase`, `strip_html`, or `slugify`",
                            )));
                        }
                    }
                }
                return Ok(());
            }
            if meta.path.is_ident("pattern") {
                // Substring pattern, not regex. The value must contain
                // the pattern string. Kept dependency-free (no regex crate).
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(s) = lit {
                    result.pattern = Some(s.value());
                }
                return Ok(());
            }
            if meta.path.is_ident("empty_is_none") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Bool(b) = lit {
                    result.empty_is_none = b.value();
                }
                return Ok(());
            }
            if meta.path.is_ident("coerce") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(s) = lit {
                    result.coerce = Some(s.value());
                }
                return Ok(());
            }
            if meta.path.is_ident("parse") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(s) = lit {
                    match s.value().as_str() {
                        "number" => result.parse = Some("number".into()),
                        other => {
                            return Err(meta.error(format!(
                                "unknown parse mode, expected `number`, got `{other}`"
                            )));
                        }
                    }
                }
                return Ok(());
            }
            if meta.path.is_ident("clamp") {
                result.clamp = true;
                return Ok(());
            }
            if meta.path.is_ident("variants") {
                // A selector's options: a zero-arg path called at
                // `describe()` time, so the schema lists the live catalog
                // instead of a frozen copy. The callee returns any
                // JSON-serializable string list.
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(s) = lit {
                    let path: Path = syn::parse_str(&s.value()).map_err(|error| {
                        meta.error(format!("invalid variants path: {error}"))
                    })?;
                    result.variants = Some(quote! { #path });
                }
                return Ok(());
            }
            // Display hints: Pine `group`/`inline`/`tooltip`/`step` ride the
            // schema as `x-group`/`x-inline`/`x-tooltip`/`x-step`, and the tab
            // pin as `x-tab`. The painters read them; the field contract
            // never does. String hints trim at read time (an empty spelling
            // degrades to absent, never to an empty section header).
            if meta.path.is_ident("tab") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(s) = lit {
                    result.tab = Some(s.value());
                }
                return Ok(());
            }
            if meta.path.is_ident("group") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(s) = lit {
                    result.group = Some(s.value());
                }
                return Ok(());
            }
            if meta.path.is_ident("tooltip") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(s) = lit {
                    result.tooltip = Some(s.value());
                }
                return Ok(());
            }
            if meta.path.is_ident("inline") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(s) = lit {
                    result.inline_group = Some(s.value());
                }
                return Ok(());
            }
            if meta.path.is_ident("step") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                let (num, _) =
                    bound_from_lit(&lit).map_err(|message| meta.error(message))?;
                result.step = Some(num);
                return Ok(());
            }
            if meta.path.is_ident("nested") {
                result.nested = true;
                return Ok(());
            }
            if meta.path.is_ident("fixed") {
                result.fixed = true;
                return Ok(());
            }
            if meta.path.is_ident("items") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Int(i) = lit {
                    let items = i.base10_parse::<usize>().map_err(|error| {
                        meta.error(format!("invalid item count: {error}"))
                    })?;
                    if items == 0 {
                        return Err(meta.error("`items` must be at least 1"));
                    }
                    result.items = Some(items);
                }
                return Ok(());
            }
            if meta.path.is_ident("visible_when") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(s) = lit {
                    let rule = s.value();
                    if !rule.contains('=') {
                        return Err(meta.error(format!(
                            "`visible_when` expects `selector=value`, got `{rule}`"
                        )));
                    }
                    result.visible_when = Some(rule);
                }
                return Ok(());
            }
            // The measured axis: a closed vocabulary, because an axis name is
            // a semantic claim a consumer may branch on (a confidence reading
            // must never deny a trade). An unknown spelling is a build error,
            // never a silent absence.
            if meta.path.is_ident("axis") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(s) = lit {
                    let axis = s.value();
                    match axis.as_str() {
                        "correctness" | "task-value" | "producer-confidence" => {
                            result.axis = Some(axis);
                        }
                        other => {
                            return Err(meta.error(format!(
                                "unknown axis `{other}`, expected `correctness`, `task-value`, or `producer-confidence`"
                            )));
                        }
                    }
                }
                return Ok(());
            }
            let path = &meta.path;
            let name = path
                .get_ident()
                .map(ToString::to_string)
                .unwrap_or_else(|| quote!(#path).to_string());
            Err(meta.error(format!(
                "unknown field attribute `{name}`, expected one of: `skip`, `desc`, `min`, `max`, `clamp`, `required`, `format`, `max_len`, `sanitize`, `pattern`, `coerce`, `parse`, `variants`, `tab`, `group`, `tooltip`, `step`, `inline`, `axis`, `empty_is_none`, `nested`, `items`, `fixed`, `visible_when`",
            )))
        })?;
    }

    Ok(result)
}

fn is_numeric_type(ty_str: &str) -> bool {
    matches!(
        ty_str,
        "u8" | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "f32"
            | "f64"
            | "usize"
            | "isize"
    )
}

fn is_integer_type(ty_str: &str) -> bool {
    matches!(
        ty_str,
        "u8" | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "usize"
            | "isize"
    )
}

fn quote_type_string(ty_str: &str) -> TokenStream2 {
    // An optional dial edits its inner type; blank clears back to unset, so
    // the schema names the number the editor edits, not the wrapper.
    let inner = strip_option_wrapper(ty_str);
    if matches!(
        inner.as_str(),
        "u8" | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "usize"
            | "isize"
    ) {
        quote! { "integer" }
    } else if matches!(inner.as_str(), "f32" | "f64") {
        quote! { "number" }
    } else if inner.as_str() == "bool" {
        quote! { "boolean" }
    } else {
        quote! { "string" }
    }
}

/// The wrapped type behind an `Option<...>` spelling, or the spelling
/// itself when it names a bare type. Single-level only: the schemas that
/// reach this helper never nest optionals. Token spelling spaces the
/// brackets out, so spaces go first.
fn strip_option_wrapper(ty_str: &str) -> String {
    let compact: String = ty_str.chars().filter(|c| !c.is_whitespace()).collect();
    for prefix in ["Option<", "std::option::Option<"] {
        if let Some(rest) = compact.strip_prefix(prefix) {
            if let Some(inner) = rest.strip_suffix('>') {
                return inner.to_owned();
            }
        }
    }
    compact
}

/// A list member's element type and declared arity.
struct ListShape {
    elem: syn::Type,
    len: usize,
    fixed: bool,
}

/// Classify `Vec<T>` / `[T; N]` for the list arms. `None` for any other type.
fn list_shape(ty: &syn::Type, meta: &FieldMeta) -> Result<Option<ListShape>, syn::Error> {
    match ty {
        syn::Type::Path(path) if path.path.segments.last().is_some_and(|s| s.ident == "Vec") => {
            let segment = path.path.segments.last().expect("checked above");
            let elem = match &segment.arguments {
                syn::PathArguments::AngleBracketed(args) => args
                    .args
                    .iter()
                    .find_map(|arg| match arg {
                        syn::GenericArgument::Type(ty) => Some(ty.clone()),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        syn::Error::new_spanned(ty, "`items` on Vec<T> requires an element type")
                    })?,
                _ => {
                    return Err(syn::Error::new_spanned(
                        ty,
                        "`items` on Vec<T> requires an element type",
                    ))
                }
            };
            let len = meta.items.ok_or_else(|| {
                syn::Error::new_spanned(ty, "`Vec<T>` needs `#[field(items = N)]`")
            })?;
            Ok(Some(ListShape {
                elem,
                len,
                fixed: false,
            }))
        }
        syn::Type::Array(array) => {
            let len = match &array.len {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Int(int),
                    ..
                }) => int.base10_parse::<usize>().map_err(|error| {
                    syn::Error::new_spanned(&array.len, format!("invalid array length: {error}"))
                })?,
                _ => {
                    return Err(syn::Error::new_spanned(
                        &array.len,
                        "`[T; N]` needs a literal length",
                    ))
                }
            };
            if let Some(declared) = meta.items {
                if declared != len {
                    return Err(syn::Error::new_spanned(
                        ty,
                        format!("`items = {declared}` disagrees with `[T; {len}]`"),
                    ));
                }
            }
            Ok(Some(ListShape {
                elem: (*array.elem).clone(),
                len,
                fixed: true,
            }))
        }
        _ => Ok(None),
    }
}

/// Snake-case spelling of a variant name (`Breakout` -> `breakout`,
/// `CrossSeries` -> `cross_series`), the wire id a tagged union switches on.
fn variant_id(ident: &syn::Ident) -> String {
    let name = ident.to_string();
    let mut out = String::new();
    for (index, ch) in name.chars().enumerate() {
        if ch.is_uppercase() {
            if index != 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Parse a value for an enum-arm scalar field. The arm derive supports the
/// same primitive surface the struct derive does; a component-valued arm
/// field is a nested member and is refused here rather than mis-parsed.
fn enum_scalar_parse(ty: &syn::Type, field_path: &str) -> TokenStream2 {
    let ty_str = quote!(#ty).to_string();
    let normalized = ty_str.replace(' ', "");
    if normalized == "String" || normalized.ends_with("::String") {
        quote! { value.to_string() }
    } else if normalized == "bool" {
        quote! {
            value.parse().map_err(|_| fluent_wvr::FieldError::Parse(
                format!("invalid bool for '{}': {}", #field_path, value)
            ))?
        }
    } else if normalized.starts_with("ArcIntern") || normalized.contains("ArcIntern") {
        quote! { fluent_wvr::ArcIntern::from(value) }
    } else if normalized.starts_with("Option<") || normalized.starts_with("std::option::Option<") {
        let inner = normalized
            .strip_prefix("Option<")
            .or_else(|| normalized.strip_prefix("std::option::Option<"))
            .and_then(|s| s.strip_suffix('>'))
            .unwrap_or("String");
        let inner_ty: TokenStream2 =
            syn::parse_str::<TokenStream2>(inner).unwrap_or_else(|_| quote! { String });
        if inner == "String" {
            quote! { if value.is_empty() { None } else { Some(value.to_string()) } }
        } else {
            quote! {
                if value.is_empty() {
                    None
                } else {
                    Some(value.parse::<#inner_ty>().map_err(|_| fluent_wvr::FieldError::Parse(
                        format!("invalid value for '{}': {}", #field_path, value)
                    ))?)
                }
            }
        }
    } else {
        quote! {
            value.parse::<#ty>().map_err(|_| fluent_wvr::FieldError::Parse(
                format!("invalid value for '{}': {}", #field_path, value)
            ))?
        }
    }
}

/// JSON-schema members for one enum-arm scalar field (the subset a union arm
/// carries: scalar values with their own metadata).
fn enum_scalar_schema(ty: &syn::Type, meta: &FieldMeta, arm: &str) -> TokenStream2 {
    let ty_str = quote!(#ty).to_string();
    let type_str = quote_type_string(&ty_str);
    let mut parts: Vec<TokenStream2> = vec![quote! { "type": #type_str }];
    if let Some(ref desc) = meta.desc {
        parts.push(quote! { "description": #desc });
    }
    let inner = strip_option_wrapper(&ty_str);
    if is_numeric_type(&inner) {
        if let Some(min) = meta.min {
            let lit = proc_macro2::Literal::f64_unsuffixed(min);
            parts.push(quote! { "minimum": #lit });
        }
        if let Some(max) = meta.max {
            let lit = proc_macro2::Literal::f64_unsuffixed(max);
            parts.push(quote! { "maximum": #lit });
        }
    }
    if let Some(ref format) = meta.format {
        parts.push(quote! { "format": #format });
    }
    if let Some(step) = meta.step {
        let lit = proc_macro2::Literal::f64_unsuffixed(step);
        parts.push(quote! { "x-step": #lit });
    }
    if let Some(ref group) = meta.group {
        parts.push(quote! { "x-group": #group });
    }
    if let Some(ref tooltip) = meta.tooltip {
        parts.push(quote! { "x-tooltip": #tooltip });
    }
    if let Some(ref axis) = meta.axis {
        parts.push(quote! { "x-axis": #axis });
    }
    let rule = format!("kind={arm}");
    quote! {
        serde_json::json!({ #(#parts),*, "x-visible-when": #rule })
    }
}

/// One named field of an enum variant.
struct ArmField {
    ident: syn::Ident,
    ty: syn::Type,
    meta: FieldMeta,
}

/// One constructible arm of a tagged union.
struct Arm {
    ident: syn::Ident,
    id: String,
    fields: Vec<ArmField>,
}

fn enum_arms(input: &DeriveInput) -> Result<Vec<Arm>, syn::Error> {
    let Data::Enum(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            input,
            "expected an enum for a tagged union",
        ));
    };
    let mut arms = Vec::new();
    for variant in &data.variants {
        let id = variant_id(&variant.ident);
        let mut fields = Vec::new();
        match &variant.fields {
            Fields::Unit => {}
            Fields::Named(named) => {
                for field in &named.named {
                    let meta = parse_field_attrs(field)?;
                    if meta.skip {
                        continue;
                    }
                    if meta.nested || meta.items.is_some() {
                        return Err(syn::Error::new_spanned(
                            field,
                            "a tagged-union arm carries scalar fields; nest a component in a field instead",
                        ));
                    }
                    fields.push(ArmField {
                        ident: field.ident.clone().expect("named field"),
                        ty: field.ty.clone(),
                        meta,
                    });
                }
            }
            Fields::Unnamed(_) => {
                return Err(syn::Error::new_spanned(
                    variant,
                    "a tagged-union arm must be a unit or struct variant",
                ));
            }
        }
        arms.push(Arm {
            ident: variant.ident.clone(),
            id,
            fields,
        });
    }
    if arms.is_empty() {
        return Err(syn::Error::new_spanned(
            input,
            "a tagged union needs at least one arm",
        ));
    }
    Ok(arms)
}

fn expand_enum_field_access(input: &DeriveInput) -> TokenStream2 {
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let arms = match enum_arms(input) {
        Ok(arms) => arms,
        Err(error) => return error.to_compile_error(),
    };

    let ids: Vec<&str> = arms.iter().map(|arm| arm.id.as_str()).collect();
    let mut arm_names: Vec<TokenStream2> = Vec::new();
    let mut set_arms: Vec<TokenStream2> = Vec::new();
    let mut get_arms: Vec<TokenStream2> = Vec::new();
    let mut match_arms: Vec<TokenStream2> = Vec::new();

    for arm in &arms {
        let id = &arm.id;
        let variant = &arm.ident;
        let field_idents: Vec<&syn::Ident> = arm.fields.iter().map(|f| &f.ident).collect();
        let dotted: Vec<String> = arm
            .fields
            .iter()
            .map(|f| format!("{}.{}", arm.id, f.ident))
            .collect();
        arm_names.push(quote! {
            {
                let mut names: Vec<&'static str> = vec!["kind"];
                #(names.push(#dotted);)*
                ::std::boxed::Box::leak(names.into_boxed_slice())
            }
        });
        if arm.fields.is_empty() {
            match_arms.push(quote! { Self::#variant => #id });
        } else {
            match_arms.push(quote! {
                Self::#variant { #(#field_idents),* } => #id
            });
        }
        let leaf_names: Vec<String> = arm.fields.iter().map(|f| f.ident.to_string()).collect();
        let setters = arm.fields.iter().zip(&leaf_names).map(|(field, leaf)| {
            let ident = &field.ident;
            let ty = &field.ty;
            let parse = enum_scalar_parse(ty, &format!("{}.{}", arm.id, field.ident));
            quote! { #leaf => { *#ident = #parse; Ok(()) } }
        });
        let getters = arm.fields.iter().zip(&leaf_names).map(|(field, leaf)| {
            let ident = &field.ident;
            let ty_str = quote!(&field.ty).to_string();
            let render = if ty_str == "String" || ty_str.ends_with("::String") {
                quote! { #ident.clone() }
            } else {
                quote! { #ident.to_string() }
            };
            quote! { #leaf => Ok(#render) }
        });
        if arm.fields.is_empty() {
            set_arms.push(quote! {
                if name.starts_with(concat!(#id, ".")) {
                    return Err(fluent_wvr::FieldError::NotFound(name.into()));
                }
            });
            get_arms.push(quote! {
                if name.starts_with(concat!(#id, ".")) {
                    return Err(fluent_wvr::FieldError::NotFound(name.into()));
                }
            });
        } else {
            set_arms.push(quote! {
                if let Some(rest) = name.strip_prefix(concat!(#id, ".")) {
                    return match self {
                        Self::#variant { #(#field_idents),* } => {
                            match rest {
                                #(#setters),*,
                                _ => Err(fluent_wvr::FieldError::NotFound(name.into())),
                            }
                        }
                        _ => Err(fluent_wvr::FieldError::NotFound(name.into())),
                    };
                }
            });
            get_arms.push(quote! {
                if let Some(rest) = name.strip_prefix(concat!(#id, ".")) {
                    return match self {
                        Self::#variant { #(#field_idents),* } => {
                            match rest {
                                #(#getters),*,
                                _ => Err(fluent_wvr::FieldError::NotFound(name.into())),
                            }
                        }
                        _ => Err(fluent_wvr::FieldError::NotFound(name.into())),
                    };
                }
            });
        }
    }

    let kind_set = arms.iter().map(|arm| {
        let id = &arm.id;
        let variant = &arm.ident;
        let construct = if arm.fields.is_empty() {
            quote! { Self::#variant }
        } else {
            let defaults = arm.fields.iter().map(|f| {
                let ident = &f.ident;
                let ty = &f.ty;
                quote! { #ident: <#ty as ::core::default::Default>::default() }
            });
            quote! { Self::#variant { #(#defaults),* } }
        };
        quote! { #id => { *self = #construct; Ok(()) } }
    });

    quote! {
        impl #impl_generics fluent_wvr::FieldAccess for #name #ty_generics #where_clause {
            #[allow(unused_variables)]
            fn set_field(&mut self, name: &str, value: &str) -> Result<(), fluent_wvr::FieldError> {
                if name == "kind" {
                    return match value {
                        #(#kind_set),*,
                        _ => Err(fluent_wvr::FieldError::Parse(
                            format!("unknown union arm {value:?}")
                        )),
                    };
                }
                #(#set_arms)*
                Err(fluent_wvr::FieldError::NotFound(name.into()))
            }

            #[allow(unused_variables)]
            fn get_field(&self, name: &str) -> Result<String, fluent_wvr::FieldError> {
                if name == "kind" {
                    return Ok(match self { #(#match_arms),* }.to_owned());
                }
                #(#get_arms)*
                Err(fluent_wvr::FieldError::NotFound(name.into()))
            }

            #[allow(unused_variables)]
            fn field_names(&self) -> &'static [&'static str] {
                static NAMES: ::std::sync::OnceLock<Vec<&'static [&'static str]>> = ::std::sync::OnceLock::new();
                let arms = NAMES.get_or_init(|| vec![#(#arm_names),*]);
                let id = match self { #(#match_arms),* };
                let index = [#(#ids),*].iter().position(|candidate| *candidate == id).unwrap_or(0);
                arms[index]
            }
        }
    }
}

fn expand_enum_describable(input: &DeriveInput) -> TokenStream2 {
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let arms = match enum_arms(input) {
        Ok(arms) => arms,
        Err(error) => return error.to_compile_error(),
    };
    let ids: Vec<&str> = arms.iter().map(|arm| arm.id.as_str()).collect();
    let mut arm_properties: Vec<TokenStream2> = Vec::new();
    let mut one_of: Vec<TokenStream2> = Vec::new();
    for arm in &arms {
        let id = &arm.id;
        let mut entries: Vec<TokenStream2> = Vec::new();
        for field in &arm.fields {
            let path = format!("{}.{}", arm.id, field.ident);
            let schema = enum_scalar_schema(&field.ty, &field.meta, arm.id.as_str());
            entries.push(quote! { properties.insert(#path.to_owned(), #schema); });
        }
        arm_properties.push(quote! { #(#entries)* });
        one_of.push(quote! {
            serde_json::json!({ "kind": #id, "x-visible-when": format!("kind={}", #id) })
        });
    }
    quote! {
        impl #impl_generics fluent_wvr::Describable for #name #ty_generics #where_clause {
            fn describe(&self) -> serde_json::Value {
                let mut properties = serde_json::Map::new();
                properties.insert(
                    "kind".to_owned(),
                    serde_json::json!({ "type": "string", "enum": [#(#ids),*], "x-kind": true }),
                );
                #(#arm_properties)*
                serde_json::json!({
                    "type": "object",
                    "properties": serde_json::Value::Object(properties),
                    "oneOf": [#(#one_of),*],
                    "required": []
                })
            }
        }
    }
}

/// Derive macro for `fluent_wvr::FieldAccess`.
///
/// Generates `set_field`, `get_field`, and `field_names` implementations
/// that intern property names via `ArcIntern<str>` for O(1) pointer-sized
/// key matching in routing boundaries.
///
/// Supports optional field attributes (closed vocabulary — anything else
/// fails expansion naming the argument):
/// - `#[field(skip)]` — exclude this field from runtime access and schema
/// - `#[field(desc = "...")]` — field description (used by `Describable`)
/// - `#[field(min = N)]` — minimum value constraint (numeric fields only)
/// - `#[field(max = N)]` — maximum value constraint (numeric fields only)
/// - `#[field(clamp)]` — clamp out-of-range numerics into `[min, max]`
///   instead of erroring; requires both bounds; leaves an `x-clamp` trace
///   in the schema
/// - `#[field(required = false)]` — opt out of the `required` array
///   (`Option<T>` fields are never required)
/// - `#[field(format = "...")]` — JSON Schema format hint (informational)
/// - `#[field(max_len = N)]` — maximum string length
/// - `#[field(sanitize = "trim")]` — sanitization mode (`trim`,
///   `lowercase`, `strip_html`, `slugify`)
/// - `#[field(pattern = "...")]` — substring pattern, enforced by
///   containment (not a regex); emitted under the JSON Schema `pattern`
///   slot by deliberate vendor interpretation, documented here
/// - `#[field(coerce = "trim,strip_quotes")]` — boundary-string shaping
///   pipeline applied before parsing
/// - `#[field(parse = "number")]` — tolerant numeric parse via
///   `coerce::parse_number`
/// - `#[field(variants = "path::to::fn")]` — live `enum` list from a
///   zero-arg path called at `describe()` time
/// - `#[field(tab = "inputs")]` — explicit settings tab pin, emitted as
///   the `x-tab` hint (`inputs`, `style`, or `visibility`); wins over the
///   painter's name heuristics
/// - `#[field(group = "Trend Ribbon")]` — section header, emitted as
///   `x-group`; schemas without groups paint exactly as before
/// - `#[field(tooltip = "...")]` — hover help, emitted as `x-tooltip`,
///   rendered after the description
/// - `#[field(step = 0.02)]` — drag sensitivity, emitted as `x-step`;
///   sliders stay continuous and never snap
/// - `#[field(inline = "adaptive")]` — shared-row marker, emitted as
///   `x-inline`; consecutive fields sharing one marker paint on one row
/// - `#[field(empty_is_none = false)]` — keep `Some("")` for empty
///   `Option<String>` input (default converts to `None`)
/// - `#[field(nested)]` — the field is itself a component: `set_field`
///   accepts dotted paths (`"child.port"`), `get_field`/`describe`
///   delegate into the child schema
/// - `#[field(items = N)]` — `Vec<T>` list member with at most `N` items;
///   each item contributes dotted paths plus a synthetic `count` writer.
///   Required on every `Vec<T>`; `[T; N]` arrays infer it (and are fixed)
/// - `#[field(fixed)]` — the list is fixed-arity (`[T; N]`, no `count`
///   writer) rather than open (`Vec<T>`)
/// - `#[field(visible_when = "kind=breakout")]` — the member is editable
///   only while the named selector holds the named value; the field layer
///   refuses writes while inactive, so visibility is never the only guard
/// - `#[field(axis = "task-value")]` — which axis the member measures
///   (`correctness`, `task-value`, or `producer-confidence`); presentation
///   members leave it unset
///
/// `version_added` / `version_removed` are planned, not implemented: using
/// them fails expansion under the closed-vocabulary rule.
#[proc_macro_derive(FieldAccess, attributes(field))]
pub fn derive_field_access(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    if matches!(input.data, Data::Enum(_)) {
        return TokenStream::from(expand_enum_field_access(&input));
    }
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let fields = match &input.data {
        Data::Struct(DataStruct {
            fields: Fields::Named(fields),
            ..
        }) => &fields.named,
        _ => {
            return syn::Error::new_spanned(
                input,
                "FieldAccess can only be derived for structs with named fields",
            )
            .to_compile_error()
            .into();
        }
    };

    let mut field_name_strs: Vec<String> = Vec::new();
    let mut dynamic_name_stmts: Vec<TokenStream2> = Vec::new();
    let mut needs_dynamic_names = false;

    let mut set_branches: Vec<TokenStream2> = Vec::new();
    let mut get_branches: Vec<TokenStream2> = Vec::new();
    for f in fields.iter() {
        let field_ident = f.ident.as_ref().unwrap();
        let field_name_str = field_ident.to_string();
        let ty = &f.ty;
        let ty_str = quote!(#ty).to_string();
        let meta = match parse_field_attrs(f) {
            Ok(meta) => meta,
            Err(error) => return error.to_compile_error().into(),
        };

        if meta.skip {
            continue;
        }

        // Nested component and list members route dotted paths into their
        // inner members. Their names are flattened once per type (the derive
        // cannot know an inner type's names at expansion time), and the
        // field layer refuses an index or arm that is not currently present.
        if meta.nested {
            needs_dynamic_names = true;
            let prefix = format!("{field_name_str}.");
            let set_branch = quote! {
                if let Some(rest) = name.strip_prefix(#prefix) {
                    return self.#field_ident.set_field(rest, value);
                }
            };
            let get_branch = quote! {
                if let Some(rest) = name.strip_prefix(#prefix) {
                    return self.#field_ident.get_field(rest);
                }
            };
            set_branches.push(set_branch);
            get_branches.push(get_branch);
            dynamic_name_stmts.push(quote! {
                for inner in <#ty as ::core::default::Default>::default().field_names() {
                    names.push(::std::boxed::Box::leak(
                        ::std::format!("{}.{}", #field_name_str, inner).into_boxed_str()
                    ));
                }
            });
            continue;
        }
        let list = match list_shape(&f.ty, &meta) {
            Ok(list) => list,
            Err(error) => return error.to_compile_error().into(),
        };
        if let Some(shape) = list {
            needs_dynamic_names = true;
            let prefix = format!("{field_name_str}.");
            let count_name = format!("{field_name_str}.count");
            let elem = &shape.elem;
            let len = shape.len;
            let count_branch = if shape.fixed {
                quote! {}
            } else {
                let max_lit = proc_macro2::Literal::usize_unsuffixed(len);
                quote! {
                    if name == #count_name {
                        let count: usize = value.parse().map_err(|_| fluent_wvr::FieldError::Parse(
                            format!("invalid count for '{}': {}", #field_name_str, value)
                        ))?;
                        if count > #max_lit {
                            return Err(fluent_wvr::FieldError::Constraint(
                                format!("{}: count {} exceeds {}", #field_name_str, count, #max_lit)
                            ));
                        }
                        if count < self.#field_ident.len() {
                            self.#field_ident.truncate(count);
                        }
                        while self.#field_ident.len() < count {
                            self.#field_ident.push(<#elem as ::core::default::Default>::default());
                        }
                        return Ok(());
                    }
                }
            };
            let count_condition = if shape.fixed {
                quote! {}
            } else {
                quote! { || name == #count_name }
            };
            let set_branch = quote! {
                if name.starts_with(#prefix) #count_condition {
                    #count_branch
                    if let Some(rest) = name.strip_prefix(#prefix) {
                        let (index, child) = rest.split_once('.').ok_or_else(|| {
                            fluent_wvr::FieldError::NotFound(name.into())
                        })?;
                        let index: usize = index.parse().map_err(|_| fluent_wvr::FieldError::NotFound(name.into()))?;
                        let slot = self.#field_ident.get_mut(index).ok_or_else(|| {
                            fluent_wvr::FieldError::NotFound(name.into())
                        })?;
                        return slot.set_field(child, value);
                    }
                    return Err(fluent_wvr::FieldError::NotFound(name.into()));
                }
            };
            let get_count = if shape.fixed {
                quote! {}
            } else {
                quote! {
                    if name == #count_name {
                        return Ok(self.#field_ident.len().to_string());
                    }
                }
            };
            let get_branch = quote! {
                if name.starts_with(#prefix) #count_condition {
                    #get_count
                    if let Some(rest) = name.strip_prefix(#prefix) {
                        let (index, child) = rest.split_once('.').ok_or_else(|| {
                            fluent_wvr::FieldError::NotFound(name.into())
                        })?;
                        let index: usize = index.parse().map_err(|_| fluent_wvr::FieldError::NotFound(name.into()))?;
                        let slot = self.#field_ident.get(index).ok_or_else(|| {
                            fluent_wvr::FieldError::NotFound(name.into())
                        })?;
                        return slot.get_field(child);
                    }
                    return Err(fluent_wvr::FieldError::NotFound(name.into()));
                }
            };
            set_branches.push(set_branch);
            get_branches.push(get_branch);
            for index in 0..len {
                dynamic_name_stmts.push(quote! {
                    for inner in <#elem as ::core::default::Default>::default().field_names() {
                        names.push(::std::boxed::Box::leak(
                            ::std::format!("{}.{}.{}", #field_name_str, #index, inner).into_boxed_str()
                        ));
                    }
                });
            }
            if !shape.fixed {
                dynamic_name_stmts.push(quote! {
                    names.push(::std::boxed::Box::leak(
                        ::std::format!("{}.count", #field_name_str).into_boxed_str()
                    ));
                });
            }
            continue;
        }

        field_name_strs.push(field_name_str.clone());
        let has_constraints = meta.min.is_some() || meta.max.is_some();

        // Detect Option<T> by checking if the type string contains "Option" with angle brackets
        let normalized_ty = ty_str.replace(' ', "");
        let is_option = normalized_ty.starts_with("Option<")
            || normalized_ty.starts_with("std::option::Option<");
        let inner_ty_str = if is_option {
            normalized_ty
                .strip_prefix("Option<")
                .or_else(|| normalized_ty.strip_prefix("std::option::Option<"))
                .and_then(|s| s.strip_suffix('>'))
                .unwrap_or("String")
                .to_string()
        } else {
            ty_str.clone()
        };
        let inner_is_numeric = is_numeric_type(&inner_ty_str);
        let inner_is_integer = is_integer_type(&inner_ty_str);
        let inner_is_string = inner_ty_str == "String"
            || inner_ty_str == "std::string::String"
            || inner_ty_str.ends_with("::String");
        let number_tolerant = meta.parse.as_deref() == Some("number") && inner_is_numeric;
        // Explicit inner-type tokens for Option<T> strict parsing (avoids the
        // empty-turbofish inference hole).
        let inner_ty_tokens: TokenStream2 =
            syn::parse_str::<TokenStream2>(&inner_ty_str).unwrap_or_else(|_| quote! { String });

        let mut parse_and_set = if is_option {
            if inner_is_string && meta.empty_is_none {
                quote! {
                    if value.is_empty() {
                        None
                    } else {
                        Some(value.to_string())
                    }
                }
            } else if inner_is_string {
                quote! {
                    Some(value.to_string())
                }
            } else if number_tolerant && inner_is_integer {
                quote! {
                    if value.is_empty() {
                        None
                    } else {
                        {
                            let n = ::fluent_wvr::coerce::parse_number(&value).ok_or_else(|| fluent_wvr::FieldError::Parse(
                                format!("invalid Option<{}> for '{}': {}", #inner_ty_str, #field_name_str, value)
                            ))?;
                            if n.fract() != 0.0 {
                                return Err(fluent_wvr::FieldError::Parse(
                                    format!("invalid Option<{}> for '{}': {}", #inner_ty_str, #field_name_str, value)
                                ));
                            }
                            Some(n as #inner_ty_tokens)
                        }
                    }
                }
            } else if number_tolerant {
                quote! {
                    if value.is_empty() {
                        None
                    } else {
                        Some(::fluent_wvr::coerce::parse_number(&value).ok_or_else(|| fluent_wvr::FieldError::Parse(
                            format!("invalid Option<{}> for '{}': {}", #inner_ty_str, #field_name_str, value)
                        ))? as #inner_ty_tokens)
                    }
                }
            } else {
                quote! {
                    if value.is_empty() {
                        None
                    } else {
                        Some(value.parse::<#inner_ty_tokens>().map_err(|_| fluent_wvr::FieldError::Parse(
                            format!("invalid Option<{}> for '{}': {}", #inner_ty_str, #field_name_str, value)
                        ))?)
                    }
                }
            }
        } else if ty_str == "String"
            || ty_str == "std::string::String"
            || ty_str.ends_with("::String")
        {
            quote! { value.into() }
        } else if ty_str == "bool" {
            quote! {
                value.parse().map_err(|_| fluent_wvr::FieldError::Parse(
                    format!("invalid bool for '{}': {}", #field_name_str, value)
                ))?
            }
        } else if ty_str.starts_with("ArcIntern") || ty_str.contains("ArcIntern") {
            quote! { fluent_wvr::ArcIntern::from(value) }
        } else if has_constraints && inner_is_numeric {
            // Strict native parse first (rejects fractions/whitespace/scientific
            // for integers via FromStr). On native-parse failure, consult f64
            // to preserve Constraint for out-of-range numerics (e.g. "65536"
            // with max 65535) while returning Parse for gibberish/fractions.
            // Fixes H3 without breaking the Constraint expectation.
            let min_lit_fb = meta.min.map(proc_macro2::Literal::f64_unsuffixed);
            let max_lit_fb = meta.max.map(proc_macro2::Literal::f64_unsuffixed);
            let min_check_fb = min_lit_fb.map(|m| {
                let e = format!("{}: value below minimum {}", field_name_str, meta.min.unwrap());
                quote! { if wide < #m { return Err(fluent_wvr::FieldError::Constraint(#e.into())); } }
            });
            let max_check_fb = max_lit_fb.map(|m| {
                let e = format!("{}: value above maximum {}", field_name_str, meta.max.unwrap());
                quote! { if wide > #m { return Err(fluent_wvr::FieldError::Constraint(#e.into())); } }
            });
            quote! {
                match value.parse::<#ty>() {
                    Ok(v) => v,
                    Err(_) => {
                        if let Ok(wide) = value.parse::<f64>() {
                            #min_check_fb
                            #max_check_fb
                        }
                        return Err(fluent_wvr::FieldError::Parse(
                            format!("invalid {} for '{}': {}", #ty_str, #field_name_str, value)
                        ));
                    }
                }
            }
        } else if number_tolerant && inner_is_integer {
            quote! {
                {
                    let n = ::fluent_wvr::coerce::parse_number(&value).ok_or_else(|| fluent_wvr::FieldError::Parse(
                        format!("invalid {} for '{}': {}", #ty_str, #field_name_str, value)
                    ))?;
                    if n.fract() != 0.0 {
                        return Err(fluent_wvr::FieldError::Parse(
                            format!("invalid {} for '{}': {}", #ty_str, #field_name_str, value)
                        ));
                    }
                    n as #ty
                }
            }
        } else if number_tolerant {
            quote! {
                ::fluent_wvr::coerce::parse_number(&value).ok_or_else(|| fluent_wvr::FieldError::Parse(
                    format!("invalid {} for '{}': {}", #ty_str, #field_name_str, value)
                ))? as #ty
            }
        } else {
            quote! {
                value.parse::<#ty>().map_err(|_| fluent_wvr::FieldError::Parse(
                    format!("invalid {} for '{}': {}", #ty_str, #field_name_str, value)
                ))?
            }
        };

        // Apply string sanitization before constraint checks.
        // Bare String: transform directly. Option<String>: transform the Some value, preserving None.
        let is_bare_string = !is_option
            && (ty_str == "String"
                || ty_str == "std::string::String"
                || ty_str.ends_with("::String"));
        if is_bare_string || (is_option && inner_is_string) {
            // Build inner String transform (sanitize + max_len + pattern) as a
            // `|s: String| -> Result<String>` shape, then apply to bare or Option.
            let mut inner_expr: Option<TokenStream2> = None;
            // sanitize
            if let Some(ref sanitize_mode) = meta.sanitize {
                let sanitize_step: TokenStream2 = match sanitize_mode.as_str() {
                    "trim" => quote! { s.trim().to_string() },
                    "lowercase" => quote! { s.to_lowercase() },
                    "strip_html" => quote! { ::fluent_wvr::strip_html(&s) },
                    "slugify" => quote! { ::fluent_wvr::slugify(&s) },
                    _ => {
                        return syn::Error::new_spanned(
                            f,
                            format!("unknown sanitize mode: {}", sanitize_mode),
                        )
                        .to_compile_error()
                        .into();
                    }
                };
                inner_expr = Some(match inner_expr {
                    None => quote! { { let s: String = s; #sanitize_step } },
                    Some(prev) => quote! { { let s: String = #prev; #sanitize_step } },
                });
            }
            if let Some(max_len) = meta.max_len {
                let max_len_lit = proc_macro2::Literal::usize_suffixed(max_len);
                let max_len_err = format!(
                    "{}: string length exceeds maximum {}",
                    field_name_str, max_len
                );
                inner_expr = Some(match inner_expr {
                    None => {
                        quote! { { let s: String = s; if s.chars().count() > #max_len_lit { return Err(fluent_wvr::FieldError::Constraint(#max_len_err.into())); } s } }
                    }
                    Some(prev) => {
                        quote! { { let s: String = #prev; if s.chars().count() > #max_len_lit { return Err(fluent_wvr::FieldError::Constraint(#max_len_err.into())); } s } }
                    }
                });
            }
            if let Some(ref pattern) = meta.pattern {
                let pattern_lit = pattern.clone();
                let pattern_err = format!(
                    "{}: value does not match pattern '{}'",
                    field_name_str, pattern
                );
                inner_expr = Some(match inner_expr {
                    None => {
                        quote! { { let s: String = s; if !s.contains(#pattern_lit) { return Err(fluent_wvr::FieldError::Constraint(#pattern_err.into())); } s } }
                    }
                    Some(prev) => {
                        quote! { { let s: String = #prev; if !s.contains(#pattern_lit) { return Err(fluent_wvr::FieldError::Constraint(#pattern_err.into())); } s } }
                    }
                });
            }
            if let Some(expr) = inner_expr {
                if is_bare_string {
                    parse_and_set = quote! {
                        {
                            let s: String = #parse_and_set;
                            #expr
                        }
                    };
                } else {
                    parse_and_set = quote! {
                        match (#parse_and_set) {
                            None => None,
                            Some(s) => Some({ let s: String = s; #expr }),
                        }
                    };
                }
            }
        }

        // Numeric bounds/clamp: bare numerics and Option numerics share native logic.
        let is_bare_numeric = !is_option && is_numeric_type(&ty_str);
        let is_opt_numeric = is_option && inner_is_numeric;
        if is_bare_numeric || is_opt_numeric {
            let min_val = meta.min;
            let max_val = meta.max;

            if meta.clamp {
                let min_lit = min_val.map(proc_macro2::Literal::f64_unsuffixed);
                let max_lit = max_val.map(proc_macro2::Literal::f64_unsuffixed);
                match (min_lit, max_lit) {
                    (Some(min), Some(max)) => {
                        // Clamp uses an f64 round-trip so negative-for-unsigned
                        // ("-5" -> 0) still clamps instead of Parse. Fractions
                        // for integers remain Parse (fract check).
                        let is_int = if is_bare_numeric {
                            is_integer_type(&ty_str)
                        } else {
                            inner_is_integer
                        };
                        if is_bare_numeric {
                            if is_int {
                                parse_and_set = quote! {
                                    {
                                        let n: f64 = value.parse().map_err(|_| fluent_wvr::FieldError::Parse(
                                            format!("invalid {} for '{}': {}", #ty_str, #field_name_str, value)
                                        ))?;
                                        if n.fract() != 0.0 {
                                            return Err(fluent_wvr::FieldError::Parse(
                                                format!("invalid {} for '{}': {}", #ty_str, #field_name_str, value)
                                            ));
                                        }
                                        (n.clamp(#min, #max)) as #ty
                                    }
                                };
                            } else {
                                parse_and_set = quote! {
                                    {
                                        let n: f64 = value.parse().map_err(|_| fluent_wvr::FieldError::Parse(
                                            format!("invalid {} for '{}': {}", #ty_str, #field_name_str, value)
                                        ))?;
                                        (n.clamp(#min, #max)) as #ty
                                    }
                                };
                            }
                        } else if is_int {
                            parse_and_set = quote! {
                                match (if value.is_empty() { None } else {
                                    let n: f64 = value.parse().map_err(|_| fluent_wvr::FieldError::Parse(
                                        format!("invalid Option<{}> for '{}': {}", #inner_ty_str, #field_name_str, value)
                                    ))?;
                                    if n.fract() != 0.0 {
                                        return Err(fluent_wvr::FieldError::Parse(
                                            format!("invalid Option<{}> for '{}': {}", #inner_ty_str, #field_name_str, value)
                                        ));
                                    }
                                    Some((n.clamp(#min, #max)) as #inner_ty_tokens)
                                }) {
                                    None => None,
                                    Some(v) => Some(v),
                                }
                            };
                        } else {
                            parse_and_set = quote! {
                                match (if value.is_empty() { None } else {
                                    let n: f64 = value.parse().map_err(|_| fluent_wvr::FieldError::Parse(
                                        format!("invalid Option<{}> for '{}': {}", #inner_ty_str, #field_name_str, value)
                                    ))?;
                                    Some((n.clamp(#min, #max)) as #inner_ty_tokens)
                                }) {
                                    None => None,
                                    Some(v) => Some(v),
                                }
                            };
                        }
                    }
                    _ => {
                        return syn::Error::new_spanned(
                            f,
                            "`clamp` requires both `min` and `max` to be specified",
                        )
                        .to_compile_error()
                        .into();
                    }
                }
            } else {
                let min_check_bare = min_val.map(|min_val| {
                    let min_lit = proc_macro2::Literal::f64_unsuffixed(min_val);
                    let min_err = format!("{}: value below minimum {}", field_name_str, min_val);
                    quote! {
                        if v < (#min_lit as #ty) {
                            return Err(fluent_wvr::FieldError::Constraint(#min_err.into()));
                        }
                    }
                });
                let max_check_bare = max_val.map(|max_val| {
                    let max_lit = proc_macro2::Literal::f64_unsuffixed(max_val);
                    let max_err = format!("{}: value above maximum {}", field_name_str, max_val);
                    quote! {
                        if v > (#max_lit as #ty) {
                            return Err(fluent_wvr::FieldError::Constraint(#max_err.into()));
                        }
                    }
                });
                let min_check_opt = min_val.map(|min_val| {
                    let min_lit = proc_macro2::Literal::f64_unsuffixed(min_val);
                    let min_err = format!("{}: value below minimum {}", field_name_str, min_val);
                    quote! {
                        if v < (#min_lit as #inner_ty_tokens) {
                            return Err(fluent_wvr::FieldError::Constraint(#min_err.into()));
                        }
                    }
                });
                let max_check_opt = max_val.map(|max_val| {
                    let max_lit = proc_macro2::Literal::f64_unsuffixed(max_val);
                    let max_err = format!("{}: value above maximum {}", field_name_str, max_val);
                    quote! {
                        if v > (#max_lit as #inner_ty_tokens) {
                            return Err(fluent_wvr::FieldError::Constraint(#max_err.into()));
                        }
                    }
                });

                if min_val.is_some() || max_val.is_some() {
                    if is_bare_numeric {
                        parse_and_set = quote! {
                            {
                                let v: #ty = #parse_and_set;
                                #min_check_bare
                                #max_check_bare
                                v
                            }
                        };
                    } else {
                        parse_and_set = quote! {
                            match (#parse_and_set) {
                                None => None,
                                Some(v) => {
                                    #min_check_opt
                                    #max_check_opt
                                    Some(v)
                                }
                            }
                        };
                    }
                }
            }
        }

        // Boundary-string coercion pipeline (`#[field(coerce = "...")]`)
        // applied before parsing/constraints: shapes the untrusted string the
        // same way `fluent_wvr::boundary` does, so both decode paths share one
        // vocabulary.
        let coerce_stmt: TokenStream2 = if let Some(ref modes) = meta.coerce {
            let mut tokens: Vec<TokenStream2> = Vec::new();
            let mut bad: Option<String> = None;
            for m in modes.split(',') {
                let m = m.trim();
                if m.is_empty() {
                    continue;
                }
                match m {
                    "trim" => tokens.push(quote! { ::fluent_wvr::coerce::Coercion::Trim }),
                    "lowercase" => {
                        tokens.push(quote! { ::fluent_wvr::coerce::Coercion::Lowercase })
                    }
                    "strip_quotes" => {
                        tokens.push(quote! { ::fluent_wvr::coerce::Coercion::StripQuotes })
                    }
                    "json_escape" => {
                        tokens.push(quote! { ::fluent_wvr::coerce::Coercion::JsonEscape })
                    }
                    "normalize_literal" => {
                        tokens.push(quote! { ::fluent_wvr::coerce::Coercion::NormalizeLiteral })
                    }
                    other => {
                        bad = Some(other.to_string());
                        break;
                    }
                }
            }
            if let Some(other) = bad {
                syn::Error::new_spanned(f, format!("unknown coerce mode: {other}"))
                    .to_compile_error()
            } else {
                quote! {
                    let value = ::fluent_wvr::coerce::coerce(value, &[#(#tokens),*]);
                }
            }
        } else {
            quote! {}
        };

        let set_expr = quote! {
            #coerce_stmt
            self.#field_ident = #parse_and_set;
            Ok(())
        };

        set_branches.push(quote! {
            if name == #field_name_str {
                #set_expr
            }
        });

        let to_string_expr = if is_option {
            let inner_is_string = inner_ty_str == "String"
                || inner_ty_str == "std::string::String"
                || inner_ty_str.ends_with("::String");
            if inner_is_string {
                quote! { self.#field_ident.as_deref().unwrap_or("").to_string() }
            } else {
                // Numeric/other Option inner types have no Deref: render the
                // contained value (or empty when None).
                quote! { self.#field_ident.map(|v| v.to_string()).unwrap_or_default() }
            }
        } else if ty_str == "String"
            || ty_str == "std::string::String"
            || ty_str.ends_with("::String")
        {
            quote! { self.#field_ident.clone() }
        } else {
            quote! { self.#field_ident.to_string() }
        };

        get_branches.push(quote! {
            if name == #field_name_str {
                Ok(#to_string_expr)
            }
        });
    }

    let mut set_body = TokenStream2::new();
    for (index, branch) in set_branches.iter().enumerate() {
        if index == 0 {
            set_body.extend(quote! { #branch });
        } else {
            set_body.extend(quote! { else #branch });
        }
    }
    let mut get_body = TokenStream2::new();
    for (index, branch) in get_branches.iter().enumerate() {
        if index == 0 {
            get_body.extend(quote! { #branch });
        } else {
            get_body.extend(quote! { else #branch });
        }
    }

    let names_impl = if needs_dynamic_names {
        quote! {
            fn field_names(&self) -> &'static [&'static str] {
                static NAMES: ::std::sync::OnceLock<&'static [&'static str]> = ::std::sync::OnceLock::new();
                NAMES.get_or_init(|| {
                    let mut names: Vec<&'static str> = Vec::new();
                    #(names.push(#field_name_strs);)*
                    #(#dynamic_name_stmts)*
                    ::std::boxed::Box::leak(names.into_boxed_slice())
                })
            }
        }
    } else {
        quote! {
            fn field_names(&self) -> &'static [&'static str] {
                static NAMES: &[&str] = &[#(#field_name_strs),*];
                NAMES
            }
        }
    };

    let set_impl = if set_branches.is_empty() {
        quote! {
            Err(fluent_wvr::FieldError::NotFound(name.into()))
        }
    } else {
        quote! {
            #set_body else {
                Err(fluent_wvr::FieldError::NotFound(name.into()))
            }
        }
    };
    let get_impl = if get_branches.is_empty() {
        quote! {
            Err(fluent_wvr::FieldError::NotFound(name.into()))
        }
    } else {
        quote! {
            #get_body else {
                Err(fluent_wvr::FieldError::NotFound(name.into()))
            }
        }
    };

    let expanded = quote! {
        impl #impl_generics fluent_wvr::FieldAccess for #name #ty_generics #where_clause {
            #[allow(unused_variables)]
            fn set_field(&mut self, name: &str, value: &str) -> Result<(), fluent_wvr::FieldError> {
                #set_impl
            }

            fn get_field(&self, name: &str) -> Result<String, fluent_wvr::FieldError> {
                #get_impl
            }

            #names_impl
        }
    };

    TokenStream::from(expanded)
}

/// Derive macro for `fluent_wvr::Describable`.
///
/// Generates a `describe()` method that returns a JSON Schema representation
/// of the struct, using `#[field(...)]` attributes for descriptions and constraints.
#[proc_macro_derive(Describable, attributes(field))]
pub fn derive_describable(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    if matches!(input.data, Data::Enum(_)) {
        return TokenStream::from(expand_enum_describable(&input));
    }
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let fields = match &input.data {
        Data::Struct(DataStruct {
            fields: Fields::Named(fields),
            ..
        }) => &fields.named,
        _ => {
            return syn::Error::new_spanned(
                input,
                "Describable can only be derived for structs with named fields",
            )
            .to_compile_error()
            .into();
        }
    };

    let mut property_stmts: Vec<TokenStream2> = Vec::new();
    let mut required = Vec::new();
    let mut schema_fields = Vec::new();

    for f in fields {
        let field_ident = f.ident.as_ref().unwrap();
        let field_name_str = field_ident.to_string();
        let ty = &f.ty;
        let ty_str = quote!(#ty).to_string();
        let meta = match parse_field_attrs(f) {
            Ok(meta) => meta,
            Err(error) => return error.to_compile_error().into(),
        };

        if meta.skip {
            continue;
        }

        let list = match list_shape(&f.ty, &meta) {
            Ok(list) => list,
            Err(error) => return error.to_compile_error().into(),
        };

        if meta.nested {
            let desc_expr = match &meta.desc {
                Some(d) => quote! { Some(#d.into()) },
                None => quote! { None },
            };
            let axis_expr = match &meta.axis {
                Some(a) => quote! { Some(#a.into()) },
                None => quote! { None },
            };
            let visible_when_expr = match &meta.visible_when {
                Some(v) => quote! { Some(#v.into()) },
                None => quote! { None },
            };
            let group_expr = match &meta.group {
                Some(g) => quote! { Some(#g) },
                None => quote! { None },
            };
            property_stmts.push(quote! {
                {
                    let inner = <#ty as ::core::default::Default>::default().describe();
                    let inner_properties = inner
                        .get("properties")
                        .and_then(serde_json::Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    properties.insert(
                        #field_name_str.to_owned(),
                        serde_json::json!({
                            "type": "object",
                            "x-flatten": true,
                            "properties": inner_properties,
                        }),
                    );
                    let group: Option<&str> = #group_expr;
                    for (key, value) in &inner_properties {
                        let mut value = value.clone();
                        if let Some(group) = group {
                            if let Some(object) = value.as_object_mut() {
                                object.insert("x-group".to_owned(), serde_json::json!(group));
                            }
                        }
                        properties.insert(
                            format!("{}.{}", #field_name_str, key),
                            value,
                        );
                    }
                }
            });
            schema_fields.push(quote! {
                fluent_wvr::FieldSchema {
                    name: #field_name_str.into(),
                    type_name: #ty_str.into(),
                    description: #desc_expr,
                    min: None,
                    max: None,
                    min_raw: None,
                    max_raw: None,
                    required: true,
                    format: None,
                    max_len: None,
                    sanitize: None,
                    pattern: None,
                    coerce: None,
                    parse: None,
                    axis: #axis_expr,
                    visible_when: #visible_when_expr,
                    items: None,
                    fixed: false,
                }
            });
            continue;
        }

        if let Some(shape) = list {
            let desc_expr = match &meta.desc {
                Some(d) => quote! { Some(#d.into()) },
                None => quote! { None },
            };
            let axis_expr = match &meta.axis {
                Some(a) => quote! { Some(#a.into()) },
                None => quote! { None },
            };
            let visible_when_expr = match &meta.visible_when {
                Some(v) => quote! { Some(#v.into()) },
                None => quote! { None },
            };
            let elem = &shape.elem;
            let len = shape.len;
            let fixed = shape.fixed;
            let group_expr = match &meta.group {
                Some(g) => quote! { Some(#g) },
                None => quote! { None },
            };
            let items_expr = quote! { Some(#len) };
            let fixed_expr = if fixed {
                quote! { true }
            } else {
                quote! { false }
            };
            property_stmts.push(quote! {
                {
                    let inner = <#elem as ::core::default::Default>::default().describe();
                    let inner_properties = inner
                        .get("properties")
                        .and_then(serde_json::Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    properties.insert(
                        #field_name_str.to_owned(),
                        serde_json::json!({
                            "type": "array",
                            "items": inner,
                            "x-items": #len,
                            "x-fixed": #fixed,
                        }),
                    );
                    let group: Option<&str> = #group_expr;
                    for index in 0..#len {
                        for (key, value) in &inner_properties {
                            let mut value = value.clone();
                            if let Some(object) = value.as_object_mut() {
                                if let Some(group) = group {
                                    object.insert("x-group".to_owned(), serde_json::json!(group));
                                }
                                object.insert(
                                    "x-visible-when".to_owned(),
                                    serde_json::json!(format!("{}.count>{}", #field_name_str, index)),
                                );
                                object.insert(
                                    "x-item-of".to_owned(),
                                    serde_json::json!(#field_name_str),
                                );
                                object.insert(
                                    "x-item-index".to_owned(),
                                    serde_json::json!(index),
                                );
                            }
                            properties.insert(
                                format!("{}.{}.{}", #field_name_str, index, key),
                                value,
                            );
                        }
                    }
                    if !#fixed {
                        properties.insert(
                            format!("{}.count", #field_name_str),
                            serde_json::json!({
                                "type": "integer",
                                "minimum": 0,
                                "maximum": #len,
                                "x-count": #field_name_str,
                            }),
                        );
                    }
                }
            });
            schema_fields.push(quote! {
                fluent_wvr::FieldSchema {
                    name: #field_name_str.into(),
                    type_name: #ty_str.into(),
                    description: #desc_expr,
                    min: None,
                    max: None,
                    min_raw: None,
                    max_raw: None,
                    required: true,
                    format: None,
                    max_len: None,
                    sanitize: None,
                    pattern: None,
                    coerce: None,
                    parse: None,
                    axis: #axis_expr,
                    visible_when: #visible_when_expr,
                    items: #items_expr,
                    fixed: #fixed_expr,
                }
            });
            continue;
        }

        let mut schema = Vec::new();

        let type_str = quote_type_string(&ty_str);
        schema.push(quote! { "type": #type_str });

        // Option<T> fields are not required
        let normalized_ty_for_desc = ty_str.replace(' ', "");
        let is_option = normalized_ty_for_desc.starts_with("Option<")
            || normalized_ty_for_desc.starts_with("std::option::Option<");
        let effective_required = if is_option { false } else { meta.required };

        if let Some(ref desc) = meta.desc {
            schema.push(quote! { "description": #desc });
        }

        if is_numeric_type(&strip_option_wrapper(&ty_str)) {
            if let Some(min) = meta.min {
                let min_lit = proc_macro2::Literal::f64_unsuffixed(min);
                schema.push(quote! { "minimum": #min_lit });
            }
            if let Some(max) = meta.max {
                let max_lit = proc_macro2::Literal::f64_unsuffixed(max);
                schema.push(quote! { "maximum": #max_lit });
            }
        }

        if let Some(ref fmt) = meta.format {
            schema.push(quote! { "format": #fmt });
        }

        if let Some(max_len) = meta.max_len {
            let max_len_lit = proc_macro2::Literal::usize_unsuffixed(max_len);
            schema.push(quote! { "maxLength": #max_len_lit });
        }

        if let Some(ref sanitize) = meta.sanitize {
            schema.push(quote! { "sanitize": #sanitize });
        }

        if let Some(ref pattern) = meta.pattern {
            // Substring-enforced, documented as such: the JSON Schema
            // `pattern` slot carries a substring, not a regex. The slot is
            // kept (additive-only contract); the enforcement never changed.
            schema.push(quote! { "pattern": #pattern });
        }

        if meta.clamp {
            // Exactly one clamp trace: `x-clamp` with the numeric bounds,
            // mirroring `minimum`/`maximum`. `clamp` requires both `min` and
            // `max` (enforced in `set_field` codegen), so both are present.
            match (meta.min, meta.max) {
                (Some(min), Some(max)) => {
                    let min_lit = proc_macro2::Literal::f64_unsuffixed(min);
                    let max_lit = proc_macro2::Literal::f64_unsuffixed(max);
                    schema.push(quote! { "x-clamp": [#min_lit, #max_lit] });
                }
                _ => {
                    return syn::Error::new_spanned(
                        f,
                        "`clamp` requires both `min` and `max` to be specified",
                    )
                    .to_compile_error()
                    .into();
                }
            }
        }

        if let Some(ref coerce) = meta.coerce {
            schema.push(quote! { "coerce": #coerce });
        }

        if let Some(ref parse) = meta.parse {
            schema.push(quote! { "parse": #parse });
        }

        if let Some(ref variants) = meta.variants {
            schema.push(quote! { "enum": #variants() });
        }

        if let Some(ref tab) = meta.tab {
            schema.push(quote! { "x-tab": #tab });
        }

        if let Some(ref group) = meta.group {
            schema.push(quote! { "x-group": #group });
        }

        if let Some(ref tooltip) = meta.tooltip {
            schema.push(quote! { "x-tooltip": #tooltip });
        }

        if let Some(step) = meta.step {
            let step_lit = proc_macro2::Literal::f64_unsuffixed(step);
            schema.push(quote! { "x-step": #step_lit });
        }

        if let Some(ref inline_group) = meta.inline_group {
            schema.push(quote! { "x-inline": #inline_group });
        }

        if let Some(ref axis) = meta.axis {
            schema.push(quote! { "x-axis": #axis });
        }

        let field_name_lit = field_name_str.clone();
        property_stmts.push(quote! {
            properties.insert(#field_name_lit.to_owned(), serde_json::json!({ #(#schema),* }));
        });

        let desc_expr = match &meta.desc {
            Some(d) => quote! { Some(#d.into()) },
            None => quote! { None },
        };
        let min_expr = match meta.min {
            Some(v) => quote! { Some(#v) },
            None => quote! { None },
        };
        let max_expr = match meta.max {
            Some(v) => quote! { Some(#v) },
            None => quote! { None },
        };
        let min_raw_expr = match &meta.min_raw {
            Some(r) => quote! { Some(#r.into()) },
            None => quote! { None },
        };
        let max_raw_expr = match &meta.max_raw {
            Some(r) => quote! { Some(#r.into()) },
            None => quote! { None },
        };
        let type_name_str = ty_str.clone();

        let required_expr = if effective_required {
            quote! { true }
        } else {
            quote! { false }
        };

        let format_expr = match &meta.format {
            Some(f) => quote! { Some(#f.into()) },
            None => quote! { None },
        };
        let max_len_expr = match meta.max_len {
            Some(v) => quote! { Some(#v) },
            None => quote! { None },
        };
        let sanitize_expr = match &meta.sanitize {
            Some(s) => quote! { Some(#s.into()) },
            None => quote! { None },
        };
        let pattern_expr = match &meta.pattern {
            Some(p) => quote! { Some(#p.into()) },
            None => quote! { None },
        };
        let coerce_expr = match &meta.coerce {
            Some(c) => quote! { Some(#c.into()) },
            None => quote! { None },
        };
        let parse_expr = match &meta.parse {
            Some(p) => quote! { Some(#p.into()) },
            None => quote! { None },
        };
        let axis_expr = match &meta.axis {
            Some(a) => quote! { Some(#a.into()) },
            None => quote! { None },
        };
        let visible_when_expr = match &meta.visible_when {
            Some(v) => quote! { Some(#v.into()) },
            None => quote! { None },
        };
        let items_expr = match meta.items {
            Some(items) => quote! { Some(#items) },
            None => quote! { None },
        };
        let fixed_expr = if meta.fixed {
            quote! { true }
        } else {
            quote! { false }
        };

        schema_fields.push(quote! {
            fluent_wvr::FieldSchema {
                name: #field_name_str.into(),
                type_name: #type_name_str.into(),
                description: #desc_expr,
                min: #min_expr,
                max: #max_expr,
                min_raw: #min_raw_expr,
                max_raw: #max_raw_expr,
                required: #required_expr,
                format: #format_expr,
                max_len: #max_len_expr,
                sanitize: #sanitize_expr,
                pattern: #pattern_expr,
                coerce: #coerce_expr,
                parse: #parse_expr,
                axis: #axis_expr,
                visible_when: #visible_when_expr,
                items: #items_expr,
                fixed: #fixed_expr,
            }
        });

        if effective_required {
            required.push(field_name_str);
        }
    }

    let expanded = quote! {
        impl #impl_generics fluent_wvr::Describable for #name #ty_generics #where_clause {
            fn describe(&self) -> serde_json::Value {
                // Imperative assembly, not one nested `json!` document:
                // the macro muncher recurses per literal, so a wide schema
                // would cross the caller's recursion budget. Each field
                // still renders through its own small literal; only the
                // document spine is built by hand. Output is identical
                // (same map type, same insertion order).
                let mut properties = serde_json::Map::new();
                #(#property_stmts)*
                let mut required_names: Vec<serde_json::Value> = Vec::new();
                #(required_names.push(#required.into());)*
                serde_json::json!({
                    "type": "object",
                    "properties": serde_json::Value::Object(properties),
                    "required": required_names
                })
            }
        }

        impl #impl_generics fluent_wvr::SchemaProvider for #name #ty_generics #where_clause {
            fn schema(&self) -> Vec<fluent_wvr::FieldSchema> {
                vec![#(#schema_fields),*]
            }
        }
    };

    TokenStream::from(expanded)
}

#[cfg(test)]
#[path = "unknown_attr_tests.rs"]
mod unknown_attr_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn make_field(attrs: &str, ty: &str, name: &str) -> syn::Field {
        let input: syn::DeriveInput = syn::parse_str(&format!(
            "struct Foo {{ #[field({})] {}: {} }}",
            attrs, name, ty
        ))
        .unwrap();
        match input.data {
            syn::Data::Struct(s) => match s.fields {
                syn::Fields::Named(named) => named.named.into_iter().next().unwrap(),
                _ => panic!("expected named fields"),
            },
            _ => panic!("expected struct"),
        }
    }

    #[test]
    fn parse_field_attrs_extracts_desc() {
        let field = make_field(r#"desc = "TCP port""#, "u16", "port");
        let meta = parse_field_attrs(&field).expect("fixture uses known attributes");
        assert_eq!(meta.desc.as_deref(), Some("TCP port"));
        assert!(meta.min.is_none());
        assert!(meta.max.is_none());
        assert!(!meta.skip);
        assert!(meta.required);
    }

    #[test]
    fn parse_field_attrs_extracts_min_max() {
        let field = make_field("min = 1, max = 65535", "u16", "port");
        let meta = parse_field_attrs(&field).expect("fixture uses known attributes");
        assert_eq!(meta.min, Some(1.0));
        assert_eq!(meta.max, Some(65535.0));
    }

    #[test]
    fn parse_field_attrs_extracts_skip() {
        let field = make_field("skip", "String", "ignored");
        let meta = parse_field_attrs(&field).expect("fixture uses known attributes");
        assert!(meta.skip);
    }

    #[test]
    fn parse_field_attrs_extracts_required_false() {
        let field = make_field("required = false", "Option<String>", "nickname");
        let meta = parse_field_attrs(&field).expect("fixture uses known attributes");
        assert!(!meta.required);
    }

    #[test]
    fn parse_field_attrs_extracts_format() {
        let field = make_field(r#"format = "url""#, "String", "endpoint");
        let meta = parse_field_attrs(&field).expect("fixture uses known attributes");
        assert_eq!(meta.format.as_deref(), Some("url"));
    }

    #[test]
    fn parse_field_attrs_extracts_max_len() {
        let field = make_field("max_len = 100", "String", "name");
        let meta = parse_field_attrs(&field).expect("fixture uses known attributes");
        assert_eq!(meta.max_len, Some(100));
    }

    #[test]
    fn parse_field_attrs_extracts_sanitize() {
        let field = make_field(r#"sanitize = "trim""#, "String", "name");
        let meta = parse_field_attrs(&field).expect("fixture uses known attributes");
        assert_eq!(meta.sanitize.as_deref(), Some("trim"));
    }

    #[test]
    fn parse_field_attrs_extracts_pattern() {
        let field = make_field(r#"pattern = "https://""#, "String", "url");
        let meta = parse_field_attrs(&field).expect("fixture uses known attributes");
        assert_eq!(meta.pattern.as_deref(), Some("https://"));
        assert!(meta.variants.is_none());
    }

    #[test]
    fn parse_field_attrs_extracts_variants_path() {
        let field = make_field(r#"variants = "crate::smoothing_ids""#, "String", "kind");
        let meta = parse_field_attrs(&field).expect("fixture uses known attributes");
        assert_eq!(
            meta.variants.map(|tokens| tokens.to_string()),
            Some("crate :: smoothing_ids".to_owned())
        );
    }

    #[test]
    fn parse_field_attrs_extracts_display_hints() {
        let field = make_field(
            r#"desc = "Low", tab = "inputs", group = "Trend Ribbon", inline = "adaptive", tooltip = "Weak-leg shading", step = 0.02"#,
            "f64",
            "alpha_low",
        );
        let meta = parse_field_attrs(&field).expect("fixture uses known attributes");
        assert_eq!(meta.tab.as_deref(), Some("inputs"));
        assert_eq!(meta.group.as_deref(), Some("Trend Ribbon"));
        assert_eq!(meta.inline_group.as_deref(), Some("adaptive"));
        assert_eq!(meta.tooltip.as_deref(), Some("Weak-leg shading"));
        assert_eq!(meta.step, Some(0.02));
    }

    #[test]
    fn parse_field_attrs_display_hints_default_absent() {
        let field = make_field(r#"desc = "Plain""#, "f64", "dial");
        let meta = parse_field_attrs(&field).expect("fixture uses known attributes");
        assert!(meta.tab.is_none());
        assert!(meta.group.is_none());
        assert!(meta.inline_group.is_none());
        assert!(meta.tooltip.is_none());
        assert!(meta.step.is_none());
    }

    #[test]
    fn parse_field_attrs_extracts_integer_step() {
        let field = make_field("step = 10", "i64", "brightness");
        let meta = parse_field_attrs(&field).expect("fixture uses known attributes");
        assert_eq!(meta.step, Some(10.0));
    }

    #[test]
    fn parse_field_attrs_extracts_empty_is_none() {
        let field = make_field("empty_is_none = false", "Option<String>", "email");
        let meta = parse_field_attrs(&field).expect("fixture uses known attributes");
        assert!(!meta.empty_is_none);
    }

    #[test]
    fn parse_field_attrs_default_empty_is_none_true() {
        let field = make_field("required = false", "Option<String>", "nickname");
        let meta = parse_field_attrs(&field).expect("fixture uses known attributes");
        assert!(meta.empty_is_none);
    }

    #[test]
    fn is_numeric_type_true_for_integers() {
        for ty in &[
            "u8", "u16", "u32", "u64", "u128", "i8", "i16", "i32", "i64", "i128", "usize", "isize",
        ] {
            assert!(is_numeric_type(ty), "expected {ty} to be numeric");
        }
    }

    #[test]
    fn is_numeric_type_true_for_floats() {
        assert!(is_numeric_type("f32"));
        assert!(is_numeric_type("f64"));
    }

    #[test]
    fn is_numeric_type_false_for_non_numeric() {
        assert!(!is_numeric_type("String"));
        assert!(!is_numeric_type("bool"));
        assert!(!is_numeric_type("Option<u32>"));
        assert!(!is_numeric_type("Vec<u8>"));
    }

    #[test]
    fn quote_type_string_maps_integers_to_integer() {
        for ty in &[
            "u8", "u16", "u32", "u64", "u128", "i8", "i16", "i32", "i64", "i128", "usize", "isize",
        ] {
            let ts = quote_type_string(ty);
            let s = ts.to_string();
            assert_eq!(
                s.trim_matches('"'),
                "integer",
                "expected {ty} -> integer, got {s}"
            );
        }
    }

    #[test]
    fn quote_type_string_maps_floats_to_number() {
        let ts = quote_type_string("f64");
        let s = ts.to_string();
        assert_eq!(s.trim_matches('"'), "number", "got {s}");
    }

    #[test]
    fn quote_type_string_maps_bool_to_boolean() {
        let ts = quote_type_string("bool");
        let s = ts.to_string();
        assert_eq!(s.trim_matches('"'), "boolean", "got {s}");
    }

    #[test]
    fn quote_type_string_maps_string_to_string() {
        let ts = quote_type_string("String");
        let s = ts.to_string();
        assert_eq!(s.trim_matches('"'), "string", "got {s}");
    }

    #[test]
    fn quote_type_string_sees_through_option() {
        for (ty, want) in [
            ("Option<f64>", "number"),
            ("Option < i64 >", "integer"),
            ("Option<bool>", "boolean"),
            ("Option<String>", "string"),
            ("std::option::Option<f32>", "number"),
        ] {
            let s = quote_type_string(ty).to_string();
            assert_eq!(s.trim_matches('"'), want, "{ty} lifts its inner type");
        }
        // Bounds ride along: an optional number still declares its floor.
        assert!(is_numeric_type(&strip_option_wrapper("Option<f64>")));
        assert!(!is_numeric_type(&strip_option_wrapper("Option<String>")));
    }
}
