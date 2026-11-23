// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.
//
// Changed for reylo-sql from sqlparser-rs 0.63.0: what only its parser, its other dialects and
// its optional features used is left out, and MySQL's statements print as MySQL reads them.

//! AST types specific to loading and unloading syntax, like one available in Snowflake which
//! contains: STAGE ddl operations, PUT upload or COPY INTO
//! See [this page](https://docs.snowflake.com/en/sql-reference/commands-data-loading) for more details.

use core::fmt;

use crate::mysql::ast::helpers::key_value_options::KeyValueOptions;
use crate::mysql::ast::{Ident, ObjectName, SelectItem};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]

/// Parameters for a named stage object used in data loading/unloading.
pub struct StageParamsObject {
    /// Optional URL for the stage.
    pub url: Option<String>,
    /// Encryption-related key/value options.
    pub encryption: KeyValueOptions,
    /// Optional endpoint string.
    pub endpoint: Option<String>,
    /// Optional storage integration identifier.
    pub storage_integration: Option<String>,
    /// Credentials for accessing the stage.
    pub credentials: KeyValueOptions,
}

/// This enum enables support for both standard SQL select item expressions
/// and Snowflake-specific ones for data loading.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]

pub enum StageLoadSelectItemKind {
    /// A standard SQL select item expression.
    SelectItem(SelectItem),
    /// A Snowflake-specific select item used for stage loading.
    StageLoadSelectItem(StageLoadSelectItem),
}

impl fmt::Display for StageLoadSelectItemKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self {
            StageLoadSelectItemKind::SelectItem(item) => write!(f, "{item}"),
            StageLoadSelectItemKind::StageLoadSelectItem(item) => write!(f, "{item}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]

/// A single item in the `SELECT` list for data loading from staged files.
pub struct StageLoadSelectItem {
    /// Optional alias for the input source.
    pub alias: Option<Ident>,
    /// Column number within the staged file (1-based).
    pub file_col_num: i32,
    /// Optional element identifier following the column reference.
    pub element: Option<Ident>,
    /// Optional alias for the item (AS clause).
    pub item_as: Option<Ident>,
}

impl fmt::Display for StageParamsObject {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if let Some(ref url) = self.url {
            write!(f, " URL='{url}'")?;
        }
        if let Some(ref storage_integration) = self.storage_integration {
            write!(f, " STORAGE_INTEGRATION={storage_integration}")?;
        }
        if let Some(ref endpoint) = self.endpoint {
            write!(f, " ENDPOINT='{endpoint}'")?;
        }
        if !self.credentials.options.is_empty() {
            write!(f, " CREDENTIALS=({})", self.credentials)?;
        }
        if !self.encryption.options.is_empty() {
            write!(f, " ENCRYPTION=({})", self.encryption)?;
        }

        Ok(())
    }
}

impl fmt::Display for StageLoadSelectItem {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if let Some(alias) = &self.alias {
            write!(f, "{alias}.")?;
        }
        write!(f, "${}", self.file_col_num)?;
        if let Some(element) = &self.element {
            write!(f, ":{element}")?;
        }
        if let Some(item_as) = &self.item_as {
            write!(f, " AS {item_as}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Eq, Ord, Hash)]

/// A command to stage files to a named stage.
pub struct FileStagingCommand {
    /// The stage to which files are being staged.

    pub stage: ObjectName,
    /// Optional file matching `PATTERN` expression.
    pub pattern: Option<String>,
}

impl fmt::Display for FileStagingCommand {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.stage)?;
        if let Some(pattern) = self.pattern.as_ref() {
            write!(f, " PATTERN='{pattern}'")?;
        }
        Ok(())
    }
}
