// Copyright (C) 2024 Quickwit, Inc.
//
// Quickwit is offered under the AGPL v3.0 and as commercial software.
// For commercial licensing, contact us at hello@quickwit.io.
//
// AGPL:
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <http://www.gnu.org/licenses/>.

use std::fmt;

use hyper::header::CONTENT_TYPE;
use quickwit_config::ConfigFormat;
use serde::{self, Deserialize, Serialize, Serializer};
use thiserror::Error;
use warp::{Filter, Rejection};

pub trait RestResponse {
    fn format(&self, body_format: BodyFormat) -> Result<Vec<u8>, ()>;
}

impl<T: serde::Serialize> RestResponse for T {
    fn format(&self, body_format: BodyFormat) -> Result<Vec<u8>, ()> {
        match body_format {
            BodyFormat::Json => serde_json::to_vec(self),
            BodyFormat::PrettyJson => serde_json::to_vec_pretty(self),
        }
        .map_err(|_| {
            tracing::error!("response serialization failed");
        })
    }
}

/// Body output format used for the REST API.
#[derive(Deserialize, Clone, Debug, Eq, PartialEq, Copy, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum BodyFormat {
    Json,
    #[default]
    PrettyJson,
}

impl BodyFormat {
    pub(crate) fn result_to_vec<T: RestResponse, E: RestResponse>(
        &self,
        result: &Result<T, E>,
    ) -> Result<Vec<u8>, ()> {
        match result {
            Ok(value) => value.format(*self),
            Err(err) => err.format(*self),
        }
    }
}

impl fmt::Display for BodyFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        match &self {
            Self::Json => write!(formatter, "json"),
            Self::PrettyJson => write!(formatter, "pretty_json"),
        }
    }
}

impl Serialize for BodyFormat {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer {
        serializer.serialize_str(&self.to_string())
    }
}

/// This struct represents a QueryString passed to
/// the REST API.
#[derive(Deserialize, Debug, Eq, PartialEq, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
struct FormatQueryString {
    /// The output format requested.
    #[serde(default)]
    pub format: BodyFormat,
}

pub(crate) fn extract_format_from_qs(
) -> impl Filter<Extract = (BodyFormat,), Error = Rejection> + Clone {
    serde_qs::warp::query::<FormatQueryString>(serde_qs::Config::default())
        .map(|format_qs: FormatQueryString| format_qs.format)
}

#[derive(Debug, Error)]
#[error(
    "request's content-type is not supported: supported media types are `application/json`, \
     `application/toml`, and `application/yaml`"
)]
pub(crate) struct UnsupportedMediaType;

impl warp::reject::Reject for UnsupportedMediaType {}

pub(crate) fn extract_config_format(
) -> impl Filter<Extract = (ConfigFormat,), Error = Rejection> + Copy {
    warp::filters::header::optional::<mime_guess::Mime>(CONTENT_TYPE.as_str()).and_then(
        |mime_opt: Option<mime_guess::Mime>| {
            if let Some(mime) = mime_opt {
                let config_format = match mime.subtype().as_str() {
                    "json" => ConfigFormat::Json,
                    "toml" => ConfigFormat::Toml,
                    "yaml" => ConfigFormat::Yaml,
                    _ => {
                        return futures::future::err(warp::reject::custom(UnsupportedMediaType));
                    }
                };
                return futures::future::ok(config_format);
            }
            futures::future::ok(ConfigFormat::Json)
        },
    )
}
