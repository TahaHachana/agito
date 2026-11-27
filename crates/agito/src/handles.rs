//! Handles to objects living in the page, and the evaluation helpers built on them.

use std::sync::Arc;

use agito_protocol::cdp::runtime;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{Error, ErrorKind, Result};
use crate::transport::Session;

/// A rectangle in CSS pixels, relative to the main frame's viewport.
#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize, Serialize)]
pub struct BoundingBox {
    /// Left edge, in CSS pixels from the left of the page.
    pub x: f64,
    /// Top edge, in CSS pixels from the top of the page.
    pub y: f64,
    /// Width in CSS pixels.
    pub width: f64,
    /// Height in CSS pixels.
    pub height: f64,
}

impl BoundingBox {
    /// The center point.
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

/// A DOM element in the page. Valid until the element's document goes away.
#[derive(Clone)]
pub struct ElementHandle {
    pub(crate) session: Arc<dyn Session>,
    pub(crate) object_id: String,
}

impl std::fmt::Debug for ElementHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ElementHandle")
            .field("object_id", &self.object_id)
            .finish()
    }
}

impl ElementHandle {
    pub(crate) fn new(session: Arc<dyn Session>, object_id: String) -> Self {
        Self { session, object_id }
    }

    /// The protocol's id for the underlying remote object.
    pub fn object_id(&self) -> &str {
        &self.object_id
    }

    /// Calls `function` (a JavaScript function source, such as `function(a) { return this.id + a }`)
    /// with the element as `this` and decodes its result.
    pub async fn evaluate<T: DeserializeOwned>(&self, function: &str, args: &[Value]) -> Result<T> {
        let result =
            call_function(&self.session, &self.object_id, function, args, true, true).await?;
        decode_value(result)
    }

    /// `textContent`.
    pub async fn text_content(&self) -> Result<Option<String>> {
        self.evaluate("function() { return this.textContent; }", &[])
            .await
    }

    /// `innerText`.
    pub async fn inner_text(&self) -> Result<String> {
        self.evaluate("function() { return this.innerText; }", &[])
            .await
    }

    /// An attribute's value, or `None` when the attribute is absent.
    pub async fn get_attribute(&self, name: &str) -> Result<Option<String>> {
        self.evaluate(
            "function(n) { return this.getAttribute(n); }",
            &[Value::from(name)],
        )
        .await
    }

    /// The element's box, or `None` when it has no size.
    pub async fn bounding_box(&self) -> Result<Option<BoundingBox>> {
        self.evaluate(
            "function() { var r = this.getBoundingClientRect(); \
             if (r.width === 0 && r.height === 0) return null; \
             return { x: r.x, y: r.y, width: r.width, height: r.height }; }",
            &[],
        )
        .await
    }

    /// Releases the remote object. The handle must not be used afterwards.
    pub async fn dispose(self) -> Result<()> {
        self.session
            .send(&runtime::ReleaseObjectParams {
                object_id: self.object_id,
            })
            .await?;
        Ok(())
    }
}

/// `Runtime.callFunctionOn` with the object as `this`. Script exceptions become
/// [`ErrorKind::Evaluation`].
pub(crate) async fn call_function(
    session: &Arc<dyn Session>,
    object_id: &str,
    function: &str,
    args: &[Value],
    return_by_value: bool,
    await_promise: bool,
) -> Result<runtime::RemoteObject> {
    let arguments = args
        .iter()
        .map(|v| runtime::CallArgument {
            value: Some(v.clone()),
            ..Default::default()
        })
        .collect();
    let result = session
        .send(&runtime::CallFunctionOnParams {
            function_declaration: function.to_owned(),
            object_id: Some(object_id.to_owned()),
            arguments: Some(arguments),
            return_by_value: Some(return_by_value),
            await_promise: Some(await_promise),
            ..Default::default()
        })
        .await?;
    if let Some(details) = result.exception_details {
        return Err(evaluation_error(&details));
    }
    Ok(result.result)
}

/// Splits a remote array of elements into handles.
pub(crate) async fn array_to_handles(
    session: &Arc<dyn Session>,
    array: runtime::RemoteObject,
) -> Result<Vec<ElementHandle>> {
    let Some(object_id) = array.object_id else {
        return Ok(Vec::new());
    };
    let props = session
        .send(&runtime::GetPropertiesParams {
            object_id: object_id.clone(),
            own_properties: Some(true),
            ..Default::default()
        })
        .await?;
    let mut indexed: Vec<(usize, ElementHandle)> = props
        .result
        .into_iter()
        .filter_map(|p| {
            let index: usize = p.name.parse().ok()?;
            let id = p.value?.object_id?;
            Some((index, ElementHandle::new(session.clone(), id)))
        })
        .collect();
    indexed.sort_by_key(|(i, _)| *i);
    let _ = session
        .send(&runtime::ReleaseObjectParams { object_id })
        .await;
    Ok(indexed.into_iter().map(|(_, h)| h).collect())
}

/// Decodes a by-value remote object into `T`; `undefined` and `null` decode as JSON `null`,
/// and decoding into `()` discards the value.
pub(crate) fn decode_value<T: DeserializeOwned>(object: runtime::RemoteObject) -> Result<T> {
    // `evaluate::<()>` means the caller does not want the result, whatever it is.
    if std::any::type_name::<T>() == "()" {
        return serde_json::from_value(Value::Null).map_err(|e| Error::other(e.to_string()));
    }
    let value = match (object.value, object.unserializable_value) {
        (Some(v), _) => v,
        (None, Some(u)) => {
            return Err(Error::other(format!(
                "the script returned {u}, which has no JSON form"
            )));
        }
        (None, None) => Value::Null,
    };
    serde_json::from_value(value.clone()).map_err(|e| {
        ErrorKind::Evaluation {
            message: format!("could not decode the script's result {value}: {e}"),
        }
        .into()
    })
}

pub(crate) fn evaluation_error(details: &runtime::ExceptionDetails) -> Error {
    let description = details
        .exception
        .as_ref()
        .and_then(|e| e.description.clone())
        .unwrap_or_else(|| details.text.clone());
    ErrorKind::Evaluation {
        message: format!("Evaluation failed: {description}"),
    }
    .into()
}

/// Serializes `value` for use as a script argument.
pub(crate) fn to_arg<T: Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value)
        .map_err(|e| Error::invalid_argument(format!("argument cannot be sent to the page: {e}")))
}
