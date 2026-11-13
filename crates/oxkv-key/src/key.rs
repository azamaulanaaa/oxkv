//! The canonical key type.
//!
//! A user key has the form `0x01 || table-name || 0x00 || component...`.
//! A system key has the form
//! `0x01 || table-name || 0x00 || 0xFF || system-id || component...`.
//!
//! The derived order on [`Key`] equals the byte order of its encoding.

use core::str;

use oxkv_contracts::keyspace::{KEY_ENCODING_VERSION, SYSTEM_MARKER, validate_system_id};
use oxkv_contracts::version::KeyEncodingVersion;

use crate::component::{Component, ComponentType};
use crate::error::KeyError;
use crate::schema::KeySchema;
use crate::table::Table;
use crate::{KeyLimit, MAX_COMPONENT_BYTES, MAX_COMPONENTS, MAX_KEY_BYTES, MAX_TABLE_NAME_BYTES};

/// The body of one key. A user key has components only. A system key has one
/// registered system identifier and components only.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Body {
    /// A user key body.
    User(Vec<Component>),
    /// A system key body.
    System { id: u8, components: Vec<Component> },
}

/// A canonical key encoding v1 value.
///
/// Construct a key with [`Key::new_user`] or [`Key::new_system`]. Decode bytes
/// with [`Key::decode`]. The constructors reject every invalid value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    table: Table,
    body: Body,
}

impl Key {
    /// Creates a user key.
    ///
    /// # Errors
    ///
    /// Returns [`KeyError::Malformed`] when the component list is empty.
    /// Returns [`KeyError::TooLarge`] when a size limit is exceeded.
    pub fn new_user(table: Table, components: Vec<Component>) -> Result<Self, KeyError> {
        let key = Self {
            table,
            body: Body::User(components),
        };
        key.encode()?;
        Ok(key)
    }

    /// Creates a system key.
    ///
    /// # Errors
    ///
    /// Returns [`KeyError::UnknownSystemId`] when the identifier is not
    /// registered. Returns [`KeyError::TooLarge`] when a size limit is
    /// exceeded.
    pub fn new_system(table: Table, id: u8, components: Vec<Component>) -> Result<Self, KeyError> {
        let key = Self {
            table,
            body: Body::System { id, components },
        };
        key.encode()?;
        Ok(key)
    }

    /// Decodes one key.
    ///
    /// The decoder checks the size limit first. It checks the version next.
    /// It rejects a non-canonical or malformed key. It never repairs a key.
    ///
    /// # Errors
    ///
    /// Returns one [`KeyError`] that names the first defect.
    pub fn decode(bytes: &[u8]) -> Result<Self, KeyError> {
        if bytes.len() > MAX_KEY_BYTES {
            return Err(KeyError::TooLarge {
                limit: KeyLimit::KeyBytes,
                found: bytes.len(),
            });
        }
        let Some((&version, rest)) = bytes.split_first() else {
            return Err(KeyError::Malformed { offset: 0 });
        };
        if version != KEY_ENCODING_VERSION {
            return Err(KeyError::UnknownVersion { version });
        }
        let delimiter = rest
            .iter()
            .position(|&byte| byte == 0x00)
            .ok_or(KeyError::Malformed {
                offset: bytes.len(),
            })?;
        let table_bytes = &rest[..delimiter];
        if table_bytes.is_empty() || table_bytes.len() > MAX_TABLE_NAME_BYTES {
            return Err(KeyError::Malformed { offset: 1 });
        }
        let table_name =
            str::from_utf8(table_bytes).map_err(|_| KeyError::Malformed { offset: 1 })?;
        let table = Table::parse(table_name).map_err(|_| KeyError::Malformed { offset: 1 })?;
        let mut cursor = 1 + delimiter + 1;
        if cursor >= bytes.len() {
            return Err(KeyError::Malformed { offset: cursor });
        }
        let body = if bytes[cursor] == SYSTEM_MARKER {
            cursor += 1;
            if cursor >= bytes.len() {
                return Err(KeyError::Malformed { offset: cursor });
            }
            let id = bytes[cursor];
            cursor += 1;
            validate_system_id(id).map_err(|_| KeyError::UnknownSystemId { id })?;
            Body::System {
                id,
                components: parse_components(bytes, cursor)?,
            }
        } else {
            Body::User(parse_components(bytes, cursor)?)
        };
        Ok(Self { table, body })
    }

    /// Decodes one key and checks its table.
    ///
    /// # Errors
    ///
    /// Returns [`KeyError::OutsideTable`] when the key belongs to another
    /// table. Returns any [`KeyError`] from [`Key::decode`].
    pub fn decode_in_table(table: &Table, bytes: &[u8]) -> Result<Self, KeyError> {
        let key = Self::decode(bytes)?;
        key.check_table(table)?;
        Ok(key)
    }

    /// Decodes one key and checks its table schema.
    ///
    /// # Errors
    ///
    /// Returns [`KeyError::OutsideTable`] when the key belongs to another
    /// table. Returns [`KeyError::NonCanonical`] when the components do not
    /// match the schema. Returns any [`KeyError`] from [`Key::decode`].
    pub fn decode_with_schema(
        table: &Table,
        schema: &KeySchema,
        bytes: &[u8],
    ) -> Result<Self, KeyError> {
        let key = Self::decode_in_table(table, bytes)?;
        key.validate_schema(schema)?;
        Ok(key)
    }

    /// Encodes the key.
    ///
    /// # Errors
    ///
    /// Returns [`KeyError::Malformed`] when the component list is empty.
    /// Returns [`KeyError::TooLarge`] when a size limit is exceeded.
    pub fn encode(&self) -> Result<Vec<u8>, KeyError> {
        let mut out = Vec::new();
        out.push(KEY_ENCODING_VERSION);
        out.extend_from_slice(self.table.as_str().as_bytes());
        out.push(0x00);
        match &self.body {
            Body::User(components) => {
                if components.is_empty() {
                    return Err(KeyError::Malformed { offset: out.len() });
                }
                write_components(&mut out, components)?;
            }
            Body::System { id, components } => {
                validate_system_id(*id).map_err(|_| KeyError::UnknownSystemId { id: *id })?;
                out.push(SYSTEM_MARKER);
                out.push(*id);
                write_components(&mut out, components)?;
            }
        }
        if out.len() > MAX_KEY_BYTES {
            return Err(KeyError::TooLarge {
                limit: KeyLimit::KeyBytes,
                found: out.len(),
            });
        }
        Ok(out)
    }

    /// Returns the table.
    #[must_use]
    pub fn table(&self) -> &Table {
        &self.table
    }

    /// Returns true when the key is a system key.
    #[must_use]
    pub fn is_system(&self) -> bool {
        matches!(self.body, Body::System { .. })
    }

    /// Returns the user components or the system payload components.
    #[must_use]
    pub fn components(&self) -> &[Component] {
        match &self.body {
            Body::User(components) | Body::System { components, .. } => components,
        }
    }

    /// Returns the user components.
    #[must_use]
    pub fn user_components(&self) -> Option<&[Component]> {
        match &self.body {
            Body::User(components) => Some(components),
            Body::System { .. } => None,
        }
    }

    /// Returns the system payload components.
    #[must_use]
    pub fn system_components(&self) -> Option<&[Component]> {
        match &self.body {
            Body::User(_) => None,
            Body::System { components, .. } => Some(components),
        }
    }

    /// Returns the system identifier.
    #[must_use]
    pub fn system_id(&self) -> Option<u8> {
        match self.body {
            Body::System { id, .. } => Some(id),
            Body::User(_) => None,
        }
    }

    /// Returns the key encoding version type.
    #[must_use]
    pub const fn encoding_version() -> KeyEncodingVersion {
        KeyEncodingVersion::new(1)
    }

    /// Checks that the key belongs to the table.
    ///
    /// # Errors
    ///
    /// Returns [`KeyError::OutsideTable`] when the key belongs to another
    /// table.
    pub fn check_table(&self, table: &Table) -> Result<(), KeyError> {
        if self.table != *table {
            return Err(KeyError::OutsideTable {
                table: table.as_str().to_owned(),
            });
        }
        Ok(())
    }

    /// Checks the components against a table schema.
    ///
    /// A system key has no user schema. A key with the wrong arity or the
    /// wrong component type is not canonical for the table.
    ///
    /// # Errors
    ///
    /// Returns [`KeyError::NonCanonical`] when the key does not match the
    /// schema.
    pub fn validate_schema(&self, schema: &KeySchema) -> Result<(), KeyError> {
        let offset = self.components_offset();
        let components = self.components();
        if self.is_system() || components.len() != schema.arity() {
            return Err(KeyError::NonCanonical { offset });
        }
        for (component, expected) in components.iter().zip(schema.types()) {
            if component.component_type() != *expected {
                return Err(KeyError::NonCanonical { offset });
            }
        }
        Ok(())
    }

    fn components_offset(&self) -> usize {
        let mut offset = 1 + self.table.as_str().len() + 1;
        if self.is_system() {
            offset += 2;
        }
        offset
    }
}

fn write_components(out: &mut Vec<u8>, components: &[Component]) -> Result<(), KeyError> {
    if components.len() > MAX_COMPONENTS {
        return Err(KeyError::TooLarge {
            limit: KeyLimit::ComponentCount,
            found: components.len(),
        });
    }
    for component in components {
        component.write(out)?;
        if out.len() > MAX_KEY_BYTES {
            return Err(KeyError::TooLarge {
                limit: KeyLimit::KeyBytes,
                found: out.len(),
            });
        }
    }
    Ok(())
}

fn parse_components(bytes: &[u8], mut cursor: usize) -> Result<Vec<Component>, KeyError> {
    let mut components = Vec::new();
    while cursor < bytes.len() {
        if components.len() == MAX_COMPONENTS {
            return Err(KeyError::TooLarge {
                limit: KeyLimit::ComponentCount,
                found: components.len() + 1,
            });
        }
        let (component, next) = parse_component(bytes, cursor)?;
        components.push(component);
        cursor = next;
    }
    Ok(components)
}

fn parse_component(bytes: &[u8], tag_offset: usize) -> Result<(Component, usize), KeyError> {
    let tag = bytes[tag_offset];
    let component_type = ComponentType::from_tag(tag).ok_or({
        if tag == SYSTEM_MARKER {
            KeyError::ReservedSystemMarker { offset: tag_offset }
        } else {
            KeyError::Malformed { offset: tag_offset }
        }
    })?;
    let mut cursor = tag_offset + 1;
    let component = match component_type {
        ComponentType::Text => {
            let (payload, next) = read_escaped(bytes, cursor)?;
            let text = String::from_utf8(payload).map_err(|error| KeyError::InvalidUtf8 {
                offset: cursor.saturating_add(error.utf8_error().valid_up_to()),
            })?;
            cursor = next;
            Component::Text(text)
        }
        ComponentType::Bytes => {
            let (payload, next) = read_escaped(bytes, cursor)?;
            cursor = next;
            Component::Bytes(payload)
        }
        ComponentType::U64 => {
            let raw = read_array::<8>(bytes, cursor)?;
            cursor += 8;
            Component::U64(u64::from_be_bytes(raw))
        }
        ComponentType::I64 => {
            let mut raw = read_array::<8>(bytes, cursor)?;
            cursor += 8;
            raw[0] ^= 0x80;
            Component::I64(i64::from_be_bytes(raw))
        }
        ComponentType::Bool => {
            let Some(&value) = bytes.get(cursor) else {
                return Err(KeyError::Malformed { offset: cursor });
            };
            let boolean = match value {
                0x00 => false,
                0x01 => true,
                _ => return Err(KeyError::Malformed { offset: cursor }),
            };
            cursor += 1;
            Component::Bool(boolean)
        }
    };
    Ok((component, cursor))
}

fn read_array<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], KeyError> {
    let end = offset
        .checked_add(N)
        .ok_or(KeyError::Malformed { offset })?;
    let slice = bytes
        .get(offset..end)
        .ok_or(KeyError::Malformed { offset })?;
    let mut array = [0u8; N];
    array.copy_from_slice(slice);
    Ok(array)
}

fn read_escaped(bytes: &[u8], mut cursor: usize) -> Result<(Vec<u8>, usize), KeyError> {
    let mut payload = Vec::new();
    loop {
        let Some(&byte) = bytes.get(cursor) else {
            return Err(KeyError::Malformed { offset: cursor });
        };
        if byte == 0x00 {
            let Some(&next) = bytes.get(cursor + 1) else {
                return Err(KeyError::Malformed { offset: cursor });
            };
            match next {
                0x00 => return Ok((payload, cursor + 2)),
                0xFF => {
                    if payload.len() >= MAX_COMPONENT_BYTES {
                        return Err(KeyError::TooLarge {
                            limit: KeyLimit::ComponentBytes,
                            found: payload.len() + 1,
                        });
                    }
                    payload.push(0x00);
                    cursor += 2;
                }
                _ => return Err(KeyError::Malformed { offset: cursor }),
            }
        } else {
            if payload.len() >= MAX_COMPONENT_BYTES {
                return Err(KeyError::TooLarge {
                    limit: KeyLimit::ComponentBytes,
                    found: payload.len() + 1,
                });
            }
            payload.push(byte);
            cursor += 1;
        }
    }
}

impl Table {
    /// Decodes one key and checks that it belongs to this table.
    ///
    /// # Errors
    ///
    /// Returns any [`KeyError`] from [`Key::decode_in_table`].
    pub fn validate_key(&self, bytes: &[u8]) -> Result<Key, KeyError> {
        Key::decode_in_table(self, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(name: &str) -> Table {
        Table::parse_user(name).expect("the table name is valid")
    }

    #[test]
    fn a_user_key_round_trips() {
        let key = Key::new_user(
            table("accounts"),
            vec![Component::text("alice"), Component::u64(42)],
        )
        .expect("the key is valid");
        let bytes = key.encode().expect("the key encodes");
        assert_eq!(Key::decode(&bytes).expect("the key decodes"), key);
    }

    #[test]
    fn a_system_key_round_trips() {
        let key = Key::new_system(
            table("accounts"),
            0x01,
            vec![Component::bytes(vec![0x00, 0x01])],
        )
        .expect("the key is valid");
        let bytes = key.encode().expect("the key encodes");
        assert_eq!(Key::decode(&bytes).expect("the key decodes"), key);
        assert!(key.is_system());
        assert_eq!(key.system_id(), Some(0x01));
    }

    #[test]
    fn a_user_key_needs_one_component() {
        assert_eq!(
            Key::new_user(table("accounts"), Vec::new()),
            Err(KeyError::Malformed { offset: 10 })
        );
    }

    #[test]
    fn an_unknown_system_id_fails() {
        assert_eq!(
            Key::new_system(table("accounts"), 0x08, Vec::new()),
            Err(KeyError::UnknownSystemId { id: 0x08 })
        );
        let bytes = b"\x01accounts\x00\xff\x08";
        assert_eq!(
            Key::decode(bytes),
            Err(KeyError::UnknownSystemId { id: 0x08 })
        );
    }

    #[test]
    fn a_later_system_marker_fails() {
        let bytes = b"\x01accounts\x00\x10a\x00\x00\xff\x01";
        assert_eq!(
            Key::decode(bytes),
            Err(KeyError::ReservedSystemMarker { offset: 14 })
        );
    }

    #[test]
    fn an_unknown_version_fails() {
        assert_eq!(
            Key::decode(b"\x02accounts\x00\x30\x00"),
            Err(KeyError::UnknownVersion { version: 0x02 })
        );
    }
}
