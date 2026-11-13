//! Table key schemas.
//!
//! A manifest declares the component type and the arity of every table key.
//! [`KeySchema`] carries that declaration. [`Key::validate_schema`] rejects a
//! key that does not match it.

use crate::component::{Component, ComponentType};
use crate::{KeyError, MAX_COMPONENTS};

/// The declared component types for one table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeySchema {
    types: Vec<ComponentType>,
}

impl KeySchema {
    /// Creates a key schema.
    ///
    /// # Errors
    ///
    /// Returns [`KeyError::Malformed`] when the schema is empty. Returns
    /// [`KeyError::TooLarge`] when the schema has more than 16 components.
    pub fn new(types: impl IntoIterator<Item = ComponentType>) -> Result<Self, KeyError> {
        let types: Vec<ComponentType> = types.into_iter().collect();
        if types.is_empty() {
            return Err(KeyError::Malformed { offset: 0 });
        }
        if types.len() > MAX_COMPONENTS {
            return Err(KeyError::TooLarge {
                limit: crate::KeyLimit::ComponentCount,
                found: types.len(),
            });
        }
        Ok(Self { types })
    }

    /// Returns the component count.
    #[must_use]
    pub fn arity(&self) -> usize {
        self.types.len()
    }

    /// Returns the declared component types.
    #[must_use]
    pub fn types(&self) -> &[ComponentType] {
        &self.types
    }

    /// Checks a component list against the schema.
    ///
    /// # Errors
    ///
    /// Returns [`KeyError::NonCanonical`] when the arity or a type differs.
    pub fn validate(&self, components: &[Component]) -> Result<(), KeyError> {
        if components.len() != self.types.len() {
            return Err(KeyError::NonCanonical { offset: 0 });
        }
        for (component, expected) in components.iter().zip(&self.types) {
            if component.component_type() != *expected {
                return Err(KeyError::NonCanonical { offset: 0 });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_schema_fails() {
        assert_eq!(
            KeySchema::new(Vec::new()),
            Err(KeyError::Malformed { offset: 0 })
        );
    }

    #[test]
    fn a_schema_with_too_many_types_fails() {
        let types = vec![ComponentType::Text; MAX_COMPONENTS + 1];
        assert_eq!(
            KeySchema::new(types),
            Err(KeyError::TooLarge {
                limit: crate::KeyLimit::ComponentCount,
                found: MAX_COMPONENTS + 1
            })
        );
    }

    #[test]
    fn a_schema_accepts_its_types_only() {
        let schema =
            KeySchema::new([ComponentType::Text, ComponentType::U64]).expect("the schema is valid");
        assert!(
            schema
                .validate(&[Component::text("alice"), Component::u64(42)])
                .is_ok()
        );
        assert_eq!(
            schema.validate(&[Component::text("alice")]),
            Err(KeyError::NonCanonical { offset: 0 })
        );
        assert_eq!(
            schema.validate(&[Component::text("alice"), Component::text("42")]),
            Err(KeyError::NonCanonical { offset: 0 })
        );
    }
}
