//! Extension fields that live inside a pass-through value, and the
//! handlers whose missing docs are an explicit list rather than a failure.

use serde_json::{Value, json};

use super::operations::Operation;
use super::source::{self, DocError};

/// An extension that has no typed property, so the operation names it.
pub(super) struct ExtensionField {
    /// The path the field has on the wire.
    pub name: &'static str,
    /// The file the description is read from.
    pub file: &'static str,
    /// An item doc, or the module paragraph containing this text.
    pub doc: FieldDoc,
    /// The item whose doc names the opt-in. Absent means the field is not opt-in.
    pub opt_in: Option<&'static str>,
}

/// Where an extension field's description is written.
pub(super) enum FieldDoc {
    /// The doc comment of this item.
    Item(&'static str),
    /// The module-doc paragraph that contains this text.
    ModuleParagraph(&'static str),
}

/// Handlers with no doc comment. Any other missing doc fails generation.
const UNDOCUMENTED: &[(&str, &str)] = &[
    ("crates/gateway/stt/api/src/batch.rs", "handler"),
    ("crates/gateway/stt/api/src/batch.rs", "error_response"),
    ("crates/gateway/stt/api/src/realtime/route.rs", "upgrade"),
];

/// The item's doc comment, or the unspecified sentence when `UNDOCUMENTED` names it.
pub(super) fn documented(file: &str, item: &str) -> Result<String, String> {
    match source::doc(file, item) {
        Ok(doc) => Ok(doc),
        Err(DocError::Undocumented) if UNDOCUMENTED.contains(&(file, item)) => {
            Ok(format!("Unspecified: `{item}` has no doc comment."))
        }
        Err(error) => Err(error.message(file, item)),
    }
}

pub(super) fn handler_doc(operation: &Operation) -> Result<String, String> {
    documented(operation.file, operation.handler)
}

pub(super) fn extension_fields(fields: &[ExtensionField]) -> Result<Vec<Value>, String> {
    fields.iter().map(extension_field).collect()
}

fn extension_field(field: &ExtensionField) -> Result<Value, String> {
    let description = match field.doc {
        FieldDoc::Item(item) => documented(field.file, item)?,
        FieldDoc::ModuleParagraph(needle) => module_paragraph(field.file, needle)?,
    };
    let opt_in = match field.opt_in {
        Some(item) => Value::String(documented(field.file, item)?),
        None => json!(false),
    };
    Ok(json!({
        "description": description,
        "name": field.name,
        "x-runvil-extension": true,
        "x-runvil-opt-in": opt_in
    }))
}

fn module_paragraph(file: &str, needle: &str) -> Result<String, String> {
    let doc = source::module_doc(file).map_err(|error| error.message(file, "module"))?;
    doc.split("\n\n")
        .find(|paragraph| paragraph.contains(needle))
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("{file}: no module paragraph contains `{needle}`"))
}

/// The sentence in `ClientId`'s doc that names the header the client chooses to send.
pub(super) fn client_opt_in(doc: &str) -> Result<String, String> {
    let flat = doc.replace('\n', " ");
    flat.split(". ")
        .find(|sentence| sentence.contains("X-Runvil-Client"))
        .map(|sentence| format!("{sentence}."))
        .ok_or_else(|| "ClientId's doc does not name X-Runvil-Client".to_owned())
}
