// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-1.1 OR LicenseRef-Slint-commercial

// cSpell: ignore rfind

use super::DocumentCache;
use crate::util::{lookup_current_element_type, map_position, with_lookup_ctx};

#[cfg(target_arch = "wasm32")]
use crate::wasm_prelude::*;
use i_slint_compiler::diagnostics::Spanned;
use i_slint_compiler::expression_tree::Expression;
use i_slint_compiler::langtype::{ElementType, Type};
use i_slint_compiler::lookup::{LookupCtx, LookupObject, LookupResult};
use i_slint_compiler::parser::{syntax_nodes, SyntaxKind, SyntaxToken};
use lsp_types::{
    CompletionClientCapabilities, CompletionItem, CompletionItemKind, InsertTextFormat, Position,
    Range, TextEdit,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub(crate) fn completion_at(
    document_cache: &mut DocumentCache,
    token: SyntaxToken,
    offset: u32,
    client_caps: Option<&CompletionClientCapabilities>,
) -> Option<Vec<CompletionItem>> {
    let node = token.parent();

    let snippet_support = client_caps
        .and_then(|caps| caps.completion_item.as_ref())
        .and_then(|caps| caps.snippet_support)
        .unwrap_or(false);

    if token.kind() == SyntaxKind::StringLiteral {
        if matches!(node.kind(), SyntaxKind::ImportSpecifier | SyntaxKind::AtImageUrl) {
            return complete_path_in_string(
                token.source_file()?.path(),
                token.text(),
                offset.checked_sub(token.text_range().start().into())?,
            )
            .map(|mut r| {
                if node.kind() == SyntaxKind::ImportSpecifier && !token.text().contains('/') {
                    let mut c =
                        CompletionItem::new_simple("std-widgets.slint".into(), String::new());

                    c.kind = Some(CompletionItemKind::FILE);
                    r.push(c)
                }
                r
            });
        }
    } else if let Some(element) = syntax_nodes::Element::new(node.clone()) {
        if token.kind() == SyntaxKind::At
            || (token.kind() == SyntaxKind::Identifier
                && token.prev_token().map_or(false, |t| t.kind() == SyntaxKind::At))
        {
            return Some(vec![CompletionItem::new_simple("children".into(), String::new())]);
        }

        return resolve_element_scope(element, document_cache).map(|mut r| {
            let mut available_types = HashSet::new();
            if snippet_support {
                for c in r.iter_mut() {
                    c.insert_text_format = Some(InsertTextFormat::SNIPPET);
                    match c.kind {
                        Some(CompletionItemKind::PROPERTY) => {
                            c.insert_text = Some(format!("{}: $1;", c.label))
                        }
                        Some(CompletionItemKind::METHOD) => {
                            c.insert_text = Some(format!("{} => {{$1}}", c.label))
                        }
                        Some(CompletionItemKind::CLASS) => {
                            available_types.insert(c.label.clone());
                            if !is_followed_by_brace(&token) {
                                c.insert_text = Some(format!("{} {{$1}}", c.label))
                            }
                        }
                        _ => (),
                    }
                }
            }

            let is_global = node
                .parent()
                .and_then(|n| n.child_text(SyntaxKind::Identifier))
                .map_or(false, |k| k == "global");

            // add keywords
            r.extend(
                [
                    ("property", "property <${1:int}> ${2:name};"),
                    ("in property", "in property <${1:int}> ${2:name};"),
                    ("in-out property", "in-out property <${1:int}> ${2:name};"),
                    ("out property", "out property <${1:int}> ${2:name};"),
                    ("private property", "private property <${1:int}> ${2:name};"),
                    ("function", "function ${1:name}($2) {\n    $0\n}"),
                    ("public function", "public function ${1:name}($2) {\n    $0\n}"),
                    ("callback", "callback ${1:name}($2);"),
                ]
                .iter()
                .map(|(kw, ins_tex)| {
                    let mut c = CompletionItem::new_simple(kw.to_string(), String::new());
                    c.kind = Some(CompletionItemKind::KEYWORD);
                    with_insert_text(c, ins_tex, snippet_support)
                }),
            );

            if !is_global {
                r.extend(
                    [
                        ("animate", "animate ${1:prop} {\n     $0\n}"),
                        ("states", "states [\n    $0\n]"),
                        ("for", "for $1 in $2: ${3:Rectangle} {\n    $0\n}"),
                        ("if", "if $1: ${2:Rectangle} {\n    $0\n}"),
                        ("@children", "@children"),
                    ]
                    .iter()
                    .map(|(kw, ins_tex)| {
                        let mut c = CompletionItem::new_simple(kw.to_string(), String::new());
                        c.kind = Some(CompletionItemKind::KEYWORD);
                        with_insert_text(c, ins_tex, snippet_support)
                    }),
                );
            }

            if !is_global && snippet_support {
                add_components_to_import(&token, document_cache, available_types, &mut r);
            }

            r
        });
    } else if let Some(n) = syntax_nodes::Binding::new(node.clone()) {
        if let Some(colon) = n.child_token(SyntaxKind::Colon) {
            if offset >= colon.text_range().end().into() {
                return with_lookup_ctx(document_cache, node, |ctx| {
                    resolve_expression_scope(ctx).map(Into::into)
                })?;
            }
        }
        if token.kind() != SyntaxKind::Identifier {
            return None;
        }
        let all = resolve_element_scope(syntax_nodes::Element::new(n.parent()?)?, document_cache)?;
        return Some(
            all.into_iter()
                .filter(|ce| ce.kind == Some(CompletionItemKind::PROPERTY))
                .collect::<Vec<_>>(),
        );
    } else if let Some(n) = syntax_nodes::TwoWayBinding::new(node.clone()) {
        if token.kind() != SyntaxKind::Identifier {
            return None;
        }
        let all = resolve_element_scope(syntax_nodes::Element::new(n.parent()?)?, document_cache)?;
        return Some(
            all.into_iter()
                .filter(|ce| ce.kind == Some(CompletionItemKind::PROPERTY))
                .collect::<Vec<_>>(),
        );
    } else if let Some(n) = syntax_nodes::CallbackConnection::new(node.clone()) {
        if token.kind() != SyntaxKind::Identifier {
            return None;
        }
        let all = resolve_element_scope(syntax_nodes::Element::new(n.parent()?)?, document_cache)?;
        return Some(
            all.into_iter()
                .filter(|ce| ce.kind == Some(CompletionItemKind::METHOD))
                .collect::<Vec<_>>(),
        );
    } else if matches!(
        node.kind(),
        SyntaxKind::Type | SyntaxKind::ArrayType | SyntaxKind::ObjectType | SyntaxKind::ReturnType
    ) {
        return resolve_type_scope(token, document_cache).map(Into::into);
    } else if syntax_nodes::PropertyDeclaration::new(node.clone()).is_some() {
        if token.kind() == SyntaxKind::LAngle {
            return resolve_type_scope(token, document_cache).map(Into::into);
        }
    } else if let Some(n) = syntax_nodes::CallbackDeclaration::new(node.clone()) {
        let paren = n.child_token(SyntaxKind::LParent)?;
        if token.token.text_range().start() >= paren.token.text_range().end() {
            return resolve_type_scope(token, document_cache).map(Into::into);
        }
    } else if matches!(
        node.kind(),
        SyntaxKind::BindingExpression
            | SyntaxKind::CodeBlock
            | SyntaxKind::ReturnStatement
            | SyntaxKind::Expression
            | SyntaxKind::FunctionCallExpression
            | SyntaxKind::SelfAssignment
            | SyntaxKind::ConditionalExpression
            | SyntaxKind::BinaryExpression
            | SyntaxKind::UnaryOpExpression
            | SyntaxKind::Array
            | SyntaxKind::AtGradient
            | SyntaxKind::StringTemplate
            | SyntaxKind::IndexExpression
    ) {
        if token.kind() == SyntaxKind::At
            || (token.kind() == SyntaxKind::Identifier
                && token.prev_token().map_or(false, |t| t.kind() == SyntaxKind::At))
        {
            return Some(
                [
                    ("tr", "tr(\"$1\")"),
                    ("image-url", "image-url(\"$1\")"),
                    ("linear-gradient", "linear-gradient($1)"),
                    ("radial-gradient", "radial-gradient(circle, $1)"),
                ]
                .into_iter()
                .map(|(label, insert)| {
                    with_insert_text(
                        CompletionItem::new_simple(label.into(), String::new()),
                        insert,
                        snippet_support,
                    )
                })
                .collect::<Vec<_>>(),
            );
        }

        return with_lookup_ctx(document_cache, node, |ctx| {
            resolve_expression_scope(ctx).map(Into::into)
        })?;
    } else if let Some(q) = syntax_nodes::QualifiedName::new(node.clone()) {
        match q.parent()?.kind() {
            SyntaxKind::Element => {
                // auto-complete the components
                let global_tr = document_cache.documents.global_type_registry.borrow();
                let tr = q
                    .source_file()
                    .and_then(|sf| document_cache.documents.get_document(sf.path()))
                    .map(|doc| &doc.local_registry)
                    .unwrap_or(&global_tr);

                let mut result = tr
                    .all_elements()
                    .into_iter()
                    .filter_map(|(k, t)| {
                        match t {
                            ElementType::Component(c) if !c.is_global() => (),
                            ElementType::Builtin(b) if !b.is_internal && !b.is_global => (),
                            _ => return None,
                        };
                        let mut c = CompletionItem::new_simple(k, "element".into());
                        c.kind = Some(CompletionItemKind::CLASS);
                        Some(c)
                    })
                    .collect::<Vec<_>>();

                drop(global_tr);

                if snippet_support {
                    let available_types = result.iter().map(|c| c.label.clone()).collect();
                    add_components_to_import(&token, document_cache, available_types, &mut result);
                }

                return Some(result);
            }
            SyntaxKind::Type => {
                return resolve_type_scope(token, document_cache).map(Into::into);
            }
            SyntaxKind::Expression => {
                return with_lookup_ctx(document_cache, node, |ctx| {
                    let it = q.children_with_tokens().filter_map(|t| t.into_token());
                    let mut it = it.skip_while(|t| {
                        t.kind() != SyntaxKind::Identifier && t.token != token.token
                    });
                    let first = it.next();
                    if first.as_ref().map_or(true, |f| f.token == token.token) {
                        return resolve_expression_scope(ctx).map(Into::into);
                    }
                    let first = i_slint_compiler::parser::normalize_identifier(first?.text());
                    let global = i_slint_compiler::lookup::global_lookup();
                    let mut expr_it = global.lookup(ctx, &first)?;
                    let mut has_dot = false;
                    for t in it {
                        has_dot |= t.kind() == SyntaxKind::Dot;
                        if t.token == token.token {
                            break;
                        };
                        if t.kind() != SyntaxKind::Identifier {
                            continue;
                        }
                        has_dot = false;
                        let str = i_slint_compiler::parser::normalize_identifier(t.text());
                        expr_it = expr_it.lookup(ctx, &str)?;
                    }
                    has_dot.then(|| {
                        let mut r = Vec::new();
                        expr_it.for_each_entry(ctx, &mut |str, expr| -> Option<()> {
                            r.push(completion_item_from_expression(str, expr));
                            None
                        });
                        r
                    })
                })?;
            }
            _ => (),
        }
    } else if node.kind() == SyntaxKind::ImportIdentifierList {
        let import = syntax_nodes::ImportSpecifier::new(node.parent()?)?;

        let path = document_cache
            .documents
            .resolve_import_path(
                Some(&token.into()),
                import.child_text(SyntaxKind::StringLiteral)?.trim_matches('\"'),
            )?
            .0;
        let doc = document_cache.documents.get_document(&path)?;
        return Some(
            doc.exports
                .iter()
                .map(|(exported_name, _)| CompletionItem {
                    label: exported_name.name.clone(),
                    ..Default::default()
                })
                .collect(),
        );
    } else if node.kind() == SyntaxKind::Document {
        let r: Vec<_> = [
            // the $1 is first in the quote so the filename can be completed before the import names
            ("import", "import { ${2:Component} } from \"${1:std-widgets.slint}\";"),
            ("component", "component ${1:Component} {\n    $0\n}"),
            ("struct", "struct ${1:Name} {\n    $0\n}"),
            ("global", "global ${1:Name} {\n    $0\n}"),
            ("export", "export { $0 }"),
            ("export component", "export component ${1:ExportedComponent} {\n    $0\n}"),
            ("export struct", "export struct ${1:Name} {\n    $0\n}"),
            ("export global", "export global ${1:Name} {\n    $0\n}"),
        ]
        .iter()
        .map(|(kw, ins_tex)| {
            let mut c = CompletionItem::new_simple(kw.to_string(), String::new());
            c.kind = Some(CompletionItemKind::KEYWORD);
            with_insert_text(c, ins_tex, snippet_support)
        })
        .collect();
        return Some(r);
    } else if node.kind() == SyntaxKind::State {
        let r: Vec<_> = [("when", "when $1: {\n    $0\n}")]
            .iter()
            .map(|(kw, ins_tex)| {
                let mut c = CompletionItem::new_simple(kw.to_string(), String::new());
                c.kind = Some(CompletionItemKind::KEYWORD);
                with_insert_text(c, ins_tex, snippet_support)
            })
            .collect();
        return Some(r);
    } else if node.kind() == SyntaxKind::PropertyAnimation {
        let global_tr = document_cache.documents.global_type_registry.borrow();
        let r = global_tr
            .property_animation_type_for_property(Type::Float32)
            .property_list()
            .into_iter()
            .map(|(k, t)| {
                let mut c = CompletionItem::new_simple(k, t.to_string());
                c.kind = Some(CompletionItemKind::PROPERTY);
                if snippet_support {
                    c.insert_text_format = Some(InsertTextFormat::SNIPPET);
                    c.insert_text = Some(format!("{}: $1;", c.label));
                }
                c
            })
            .collect::<Vec<_>>();
        return Some(r);
    }
    None
}

fn with_insert_text(
    mut c: CompletionItem,
    ins_text: &str,
    snippet_support: bool,
) -> CompletionItem {
    if snippet_support {
        c.insert_text_format = Some(InsertTextFormat::SNIPPET);
        c.insert_text = Some(ins_text.to_string());
    }
    c
}

fn resolve_element_scope(
    element: syntax_nodes::Element,
    document_cache: &DocumentCache,
) -> Option<Vec<CompletionItem>> {
    let global_tr = document_cache.documents.global_type_registry.borrow();
    let tr = element
        .source_file()
        .and_then(|sf| document_cache.documents.get_document(sf.path()))
        .map(|doc| &doc.local_registry)
        .unwrap_or(&global_tr);
    let element_type = lookup_current_element_type((*element).clone(), tr).unwrap_or_default();
    let mut result = element_type
        .property_list()
        .into_iter()
        .map(|(k, t)| {
            let mut c = CompletionItem::new_simple(k, t.to_string());
            c.kind = Some(if matches!(t, Type::InferredCallback | Type::Callback { .. }) {
                CompletionItemKind::METHOD
            } else {
                CompletionItemKind::PROPERTY
            });
            c
        })
        .chain(element.PropertyDeclaration().map(|pr| {
            let mut c = CompletionItem::new_simple(
                i_slint_compiler::parser::identifier_text(&pr.DeclaredIdentifier())
                    .unwrap_or_default(),
                pr.Type().map(|t| t.text().into()).unwrap_or_else(|| "property".to_owned()),
            );
            c.kind = Some(CompletionItemKind::PROPERTY);
            c
        }))
        .chain(element.CallbackDeclaration().map(|cd| {
            let mut c = CompletionItem::new_simple(
                i_slint_compiler::parser::identifier_text(&cd.DeclaredIdentifier())
                    .unwrap_or_default(),
                "callback".into(),
            );
            c.kind = Some(CompletionItemKind::METHOD);
            c
        }))
        .collect::<Vec<_>>();

    if !matches!(element_type, ElementType::Global) {
        result.extend(
            i_slint_compiler::typeregister::reserved_properties()
                .filter_map(|(k, t)| {
                    if matches!(t, Type::Function { .. }) {
                        return None;
                    }
                    let mut c = CompletionItem::new_simple(k.into(), t.to_string());
                    c.kind = Some(if matches!(t, Type::InferredCallback | Type::Callback { .. }) {
                        CompletionItemKind::METHOD
                    } else {
                        CompletionItemKind::PROPERTY
                    });
                    Some(c)
                })
                .chain(tr.all_elements().into_iter().filter_map(|(k, t)| {
                    match t {
                        ElementType::Component(c) if !c.is_global() => (),
                        ElementType::Builtin(b) if !b.is_internal && !b.is_global => (),
                        _ => return None,
                    };
                    let mut c = CompletionItem::new_simple(k, "element".into());
                    c.kind = Some(CompletionItemKind::CLASS);
                    Some(c)
                })),
        );
    };
    Some(result)
}

fn resolve_expression_scope(lookup_context: &LookupCtx) -> Option<Vec<CompletionItem>> {
    let mut r = Vec::new();
    let global = i_slint_compiler::lookup::global_lookup();
    global.for_each_entry(lookup_context, &mut |str, expr| -> Option<()> {
        if str != "SlintInternal" {
            r.push(completion_item_from_expression(str, expr));
        }
        None
    });
    Some(r)
}

fn completion_item_from_expression(str: &str, lookup_result: LookupResult) -> CompletionItem {
    match lookup_result {
        LookupResult::Expression { expression, .. } => {
            let mut c = CompletionItem::new_simple(str.to_string(), expression.ty().to_string());
            c.kind = match expression {
                Expression::BoolLiteral(_) => Some(CompletionItemKind::CONSTANT),
                Expression::CallbackReference(..) => Some(CompletionItemKind::METHOD),
                Expression::FunctionReference(..) => Some(CompletionItemKind::FUNCTION),
                Expression::PropertyReference(_) => Some(CompletionItemKind::PROPERTY),
                Expression::BuiltinFunctionReference(..) => Some(CompletionItemKind::FUNCTION),
                Expression::BuiltinMacroReference(..) => Some(CompletionItemKind::FUNCTION),
                Expression::ElementReference(_) => Some(CompletionItemKind::CLASS),
                Expression::RepeaterIndexReference { .. } => Some(CompletionItemKind::VARIABLE),
                Expression::RepeaterModelReference { .. } => Some(CompletionItemKind::VARIABLE),
                Expression::FunctionParameterReference { .. } => Some(CompletionItemKind::VARIABLE),
                Expression::Cast { .. } => Some(CompletionItemKind::CONSTANT),
                Expression::EasingCurve(_) => Some(CompletionItemKind::CONSTANT),
                Expression::EnumerationValue(_) => Some(CompletionItemKind::ENUM_MEMBER),
                _ => None,
            };
            c
        }
        LookupResult::Enumeration(e) => {
            let mut c = CompletionItem::new_simple(str.to_string(), e.name.clone());
            c.kind = Some(CompletionItemKind::ENUM);
            c
        }
        LookupResult::Namespace(_) => CompletionItem {
            label: str.to_string(),
            kind: Some(CompletionItemKind::MODULE),
            ..CompletionItem::default()
        },
    }
}

fn resolve_type_scope(
    token: SyntaxToken,
    document_cache: &DocumentCache,
) -> Option<Vec<CompletionItem>> {
    let global_tr = document_cache.documents.global_type_registry.borrow();
    let tr = token
        .source_file()
        .and_then(|sf| document_cache.documents.get_document(sf.path()))
        .map(|doc| &doc.local_registry)
        .unwrap_or(&global_tr);
    Some(
        tr.all_types()
            .into_iter()
            .filter_map(|(k, t)| {
                t.is_property_type().then(|| {
                    let mut c = CompletionItem::new_simple(k, String::new());
                    c.kind = Some(CompletionItemKind::TYPE_PARAMETER);
                    c
                })
            })
            .collect(),
    )
}

fn complete_path_in_string(base: &Path, text: &str, offset: u32) -> Option<Vec<CompletionItem>> {
    if offset as usize > text.len() || offset == 0 {
        return None;
    }
    let mut text = text.strip_prefix('\"')?;
    text = &text[..(offset - 1) as usize];
    let base = i_slint_compiler::typeloader::base_directory(base);
    let path = if let Some(last_slash) = text.rfind('/') {
        base.join(Path::new(&text[..last_slash]))
    } else {
        base
    };
    let dir = std::fs::read_dir(path).ok()?;
    Some(
        dir.filter_map(|x| {
            let entry = x.ok()?;
            let mut c =
                CompletionItem::new_simple(entry.file_name().into_string().ok()?, String::new());
            if entry.file_type().ok()?.is_dir() {
                c.kind = Some(CompletionItemKind::FOLDER);
                c.insert_text = Some(format!("{}/", c.label));
            } else {
                c.kind = Some(CompletionItemKind::FILE);
            }
            Some(c)
        })
        .collect(),
    )
}

/// Add the components that are available when adding import to the `result`
///
/// `available_types`  are the component which are already available and need no
/// import and should already be in result
fn add_components_to_import(
    token: &SyntaxToken,
    document_cache: &mut DocumentCache,
    mut available_types: HashSet<String>,
    result: &mut Vec<CompletionItem>,
) -> Option<()> {
    // Find out types that can be imported
    let current_file = token.source_file.path().to_owned();
    let current_uri = lsp_types::Url::from_file_path(&current_file).ok()?;
    let current_doc = document_cache.documents.get_document(&current_file)?.node.as_ref()?;
    let mut import_locations = HashMap::new();
    let mut last = 0u32;
    for import in current_doc.ImportSpecifier() {
        if let Some((loc, file)) = import.ImportIdentifierList().and_then(|list| {
            let id = list.ImportIdentifier().last()?;
            Some((
                map_position(id.source_file()?, id.text_range().end()),
                import.child_token(SyntaxKind::StringLiteral)?,
            ))
        }) {
            import_locations.insert(file.text().to_string().trim_matches('\"').to_string(), loc);
        }
        last = import.text_range().end().into();
    }

    let new_import_position = if last == 0 {
        // There are currently no input statement, place it at the location of the first non-empty token.
        // This should also work in the slint! macro.
        // consider this file:  We want to insert before the doc1 position
        // ```
        // //not doc (eg, license header)
        //
        // //doc1
        // //doc2
        // component Foo {
        // ```
        let mut offset = None;
        for it in current_doc.children_with_tokens() {
            match it.kind() {
                SyntaxKind::Comment => {
                    if offset.is_none() {
                        offset = Some(it.text_range().start());
                    }
                }
                SyntaxKind::Whitespace => {
                    // Single newline is just considered part of the comment
                    // but more new lines means it splits that comment
                    if it.as_token().unwrap().text() != "\n" {
                        offset = None;
                    }
                }
                _ => {
                    if offset.is_none() {
                        offset = Some(it.text_range().start());
                    }
                    break;
                }
            }
        }
        map_position(&token.source_file, offset.unwrap_or_default())
    } else {
        Position::new(map_position(&token.source_file, last.into()).line + 1, 0)
    };

    for file in document_cache.documents.all_files() {
        let Some(doc) = document_cache.documents.get_document(file) else { continue };
        let file = if file.starts_with("builtin:/") {
            match file.file_name() {
                Some(file) if file == "std-widgets.slint" => "std-widgets.slint".into(),
                _ => continue,
            }
        } else {
            match lsp_types::Url::make_relative(
                &current_uri,
                &lsp_types::Url::from_file_path(file)
                    .unwrap_or_else(|()| panic!("Cannot parse URL for file '{file:?}'")),
            ) {
                Some(file) => file,
                None => continue,
            }
        };

        for (exported_name, ty) in &*doc.exports {
            if available_types.contains(&exported_name.name) {
                continue;
            }
            if let Some(c) = ty.as_ref().left() {
                if c.is_global() {
                    continue;
                }
            } else {
                continue;
            }
            available_types.insert(exported_name.name.clone());
            let the_import = import_locations.get(&file).map_or_else(
                || {
                    TextEdit::new(
                        Range::new(new_import_position, new_import_position),
                        format!("import {{ {} }} from \"{}\";\n", exported_name.name, file),
                    )
                },
                |pos| TextEdit::new(Range::new(*pos, *pos), format!(", {}", exported_name.name)),
            );
            result.push(CompletionItem {
                label: format!("{} (import from \"{}\")", exported_name.name, file),
                insert_text: if is_followed_by_brace(token) {
                    Some(exported_name.name.clone())
                } else {
                    Some(format!("{} {{$1}}", exported_name.name))
                },
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                filter_text: Some(exported_name.name.clone()),
                kind: Some(CompletionItemKind::CLASS),
                detail: Some(format!("(import from \"{}\")", file)),
                additional_text_edits: Some(vec![the_import]),
                ..Default::default()
            });
        }
    }
    Some(())
}

fn is_followed_by_brace(token: &SyntaxToken) -> bool {
    let mut next_token = token.next_token();
    while let Some(ref t) = next_token {
        if t.kind() != SyntaxKind::Whitespace {
            break;
        }
        next_token = t.next_token();
    }
    next_token.is_some_and(|x| x.kind() == SyntaxKind::LBrace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::uri_to_file;

    /// Given a source text containing the unicode emoji `🔺`, the emoji will be removed and then an autocompletion request will be done as if the cursor was there
    fn get_completions(file: &str) -> Option<Vec<CompletionItem>> {
        const CURSOR_EMOJI: char = '🔺';
        let offset = file.find(CURSOR_EMOJI).unwrap() as u32;
        let source = file.replace(CURSOR_EMOJI, "");
        let (mut dc, uri, _) = crate::language::test::loaded_document_cache(source);

        let doc = dc.documents.get_document(&uri_to_file(&uri).unwrap()).unwrap();
        let token = crate::language::token_at_offset(doc.node.as_ref().unwrap(), offset)?;
        let caps = CompletionClientCapabilities {
            completion_item: Some(lsp_types::CompletionItemCapability {
                snippet_support: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };

        completion_at(&mut dc, token, offset, Some(&caps))
    }

    #[test]
    fn in_expression() {
        let with_semi = r#"
            component Bar inherits Text { nope := Rectangle {} property <string> red; }
            global Glib { property <int> gama; }
            component Foo {
                property <int> alpha;
                pure function funi() {}
                bobo := Bar {
                    property <int> beta;
                    width: 🔺;
                }
            }
        "#;
        let without_semi = r#"
            component Bar inherits Text { nope := Rectangle {} property <string> red; }
            global Glib { property <int> gama; }
            component Foo {
                property <int> alpha;
                pure function funi() {}
                bobo := Bar {
                    property <int> beta;
                    width: 🔺
                }
            }
        "#;
        for source in [with_semi, without_semi] {
            let res = get_completions(source).unwrap();
            res.iter().find(|ci| ci.label == "alpha").unwrap();
            res.iter().find(|ci| ci.label == "beta").unwrap();
            res.iter().find(|ci| ci.label == "funi").unwrap();
            res.iter().find(|ci| ci.label == "Glib").unwrap();
            res.iter().find(|ci| ci.label == "Colors").unwrap();
            res.iter().find(|ci| ci.label == "Math").unwrap();
            res.iter().find(|ci| ci.label == "animation-tick").unwrap();
            res.iter().find(|ci| ci.label == "bobo").unwrap();
            res.iter().find(|ci| ci.label == "true").unwrap();
            res.iter().find(|ci| ci.label == "self").unwrap();
            res.iter().find(|ci| ci.label == "root").unwrap();

            assert!(!res.iter().any(|ci| ci.label == "text"));
            assert!(!res.iter().any(|ci| ci.label == "red"));
            assert!(!res.iter().any(|ci| ci.label == "nope"));
        }
    }

    #[test]
    fn arguments_struct() {
        let source = r#"
            struct S1 { foo: int, bar: {xx: int, yy: string} }
            component Bar { callback c(S1) }
            component Foo {
                Bar {
                    c(param) => { param.bar.🔺 }
                }
            }
        "#;
        let res = get_completions(source).unwrap();
        res.iter().find(|ci| ci.label == "xx").unwrap();
        res.iter().find(|ci| ci.label == "yy").unwrap();
        assert_eq!(res.len(), 2);
    }

    #[test]
    fn function_args() {
        let source = r#"
            component Foo {
                function xxx(alpha: int, beta_gamma: string) -> color {
                    🔺
                }
            }
        "#;
        let res = get_completions(source).unwrap();
        res.iter().find(|ci| ci.label == "alpha").unwrap();
        res.iter().find(|ci| ci.label == "beta-gamma").unwrap();
        res.iter().find(|ci| ci.label == "red").unwrap();
        assert!(!res.iter().any(|ci| ci.label == "width"));
    }

    #[test]
    fn function_no_when_in_empty_state() {
        let source = r#"
            component Foo {
                states [
                    🔺
                ]
            }
        "#;
        assert!(get_completions(source).is_none());
    }

    #[test]
    fn function_no_when_in_state() {
        let source = r#"
            component Foo {
                property<bool> bar: false;
                states [
                    foo when root.bar: { }
                    🔺
                    baz when !root.bar: { }
                ]
            }
        "#;
        assert!(get_completions(source).is_none());
    }

    #[test]
    fn function_when_after_state_name() {
        let source = r#"
            component Foo {
                states [
                    foo 🔺
                ]
            }
        "#;
        let res = get_completions(source).unwrap();
        res.iter().find(|ci| ci.label == "when").unwrap();
    }

    #[test]
    fn function_when_after_state_name_between_more_states() {
        let source = r#"
            component Foo {
                states [
                    foo when root.bar: { }
                    barbar 🔺
                    baz when !root.bar: { }
                ]
            }
        "#;
        let res = get_completions(source).unwrap();
        res.iter().find(|ci| ci.label == "when").unwrap();
    }

    #[test]
    fn import_component() {
        let source = r#"
            import {🔺} from "std-widgets.slint"
        "#;
        let res = get_completions(source).unwrap();
        res.iter().find(|ci| ci.label == "LineEdit").unwrap();

        let source = r#"
            import { Foo, 🔺} from "std-widgets.slint"
        "#;
        let res = get_completions(source).unwrap();
        res.iter().find(|ci| ci.label == "TextEdit").unwrap();
    }

    #[test]
    fn animation_completion() {
        let source = r#"
            component Foo {
                Text {
                    width: 20px;
                    animate width {
                        🔺
                    }
                }
            }
        "#;
        let res = get_completions(source).unwrap();
        res.iter().find(|ci| ci.label == "delay").unwrap();
        res.iter().find(|ci| ci.label == "duration").unwrap();
        res.iter().find(|ci| ci.label == "iteration-count").unwrap();
        res.iter().find(|ci| ci.label == "easing").unwrap();
    }

    #[test]
    fn animation_easing_completion() {
        let source = r#"
            component Foo {
                Text {
                    width: 20px;
                    animate width {
                        easing: 🔺;
                    }
                }
            }
        "#;
        let res = get_completions(source).unwrap();
        res.iter().find(|ci| ci.label == "ease").unwrap();
        res.iter().find(|ci| ci.label == "ease-in").unwrap();
        res.iter().find(|ci| ci.label == "ease-out").unwrap();
        res.iter().find(|ci| ci.label == "ease-in-out").unwrap();
        res.iter().find(|ci| ci.label == "linear").unwrap();
        res.iter().find(|ci| ci.label == "cubic-bezier").unwrap();
    }

    #[test]
    fn element_snippet_without_braces() {
        let source = r#"
            component Foo {
                🔺
            }
        "#;
        let res = get_completions(source)
            .unwrap()
            .into_iter()
            .filter(|ci| {
                matches!(
                    ci,
                    CompletionItem {
                        insert_text_format: Some(InsertTextFormat::SNIPPET),
                        detail: Some(detail),
                        ..
                    }
                    if detail == "element"
                )
            })
            .collect::<Vec<_>>();
        assert!(!res.is_empty());
        assert!(res.iter().all(|ci| ci.insert_text.as_ref().is_some_and(|t| t.ends_with("{$1}"))));
    }

    #[test]
    fn element_snippet_before_braces() {
        let source = r#"
            component Foo {
                🔺 {}
            }
        "#;
        let res = get_completions(source)
            .unwrap()
            .into_iter()
            .filter(|ci| {
                matches!(
                    ci,
                    CompletionItem {
                        insert_text_format: Some(InsertTextFormat::SNIPPET),
                        detail: Some(detail),
                        ..
                    }
                    if detail == "element"
                )
            })
            .collect::<Vec<_>>();
        assert!(!res.is_empty());
        assert!(res.iter().all(|ci| ci.insert_text.is_none()));
    }
}
