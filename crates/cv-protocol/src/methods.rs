//! The typed method contract: one
//! uninhabited marker per [`Request`] variant binding the wire name, the params type and the
//! result type. Clients call `call::<methods::X>(params)`; the daemon dispatches each variant to
//! a handler whose return type is `X::Result`; [`METHOD_NAMES`] is the server's known-method set.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::*;

/// One RPC method: its wire name, params and result.
pub trait Method {
    /// The wire `method` string.
    const NAME: &'static str;
    /// The `params` (`()` for a method without params).
    type Params: Serialize + DeserializeOwned;
    /// The `result`.
    type Result: Serialize + DeserializeOwned;
    /// Wraps `params` into the [`Request`] the wire carries (so the bytes are the enum's own).
    fn request(params: Self::Params) -> Request;
}

macro_rules! methods {
    ($( $variant:ident = $name:literal $( ($params:ty) )? -> $result:ty; )*) => {
        $(
            #[doc = concat!("`", $name, "`")]
            #[derive(Debug, Clone, Copy)]
            pub enum $variant {}
            impl Method for $variant {
                const NAME: &'static str = $name;
                type Params = methods!(@params $($params)?);
                type Result = $result;
                fn request(params: Self::Params) -> Request {
                    methods!(@wrap $variant params $($params)?)
                }
            }
        )*
        /// Every method name the daemon knows (generated with the markers).
        pub const METHOD_NAMES: &[&str] = &[$($name),*];

        impl Request {
            /// This request's wire `method` (exhaustive: a new variant needs a marker).
            #[must_use]
            pub fn method(&self) -> &'static str {
                match self {
                    $( Request::$variant { .. } => $name, )*
                }
            }
        }
    };
    (@params) => { () };
    (@params $p:ty) => { $p };
    (@wrap $variant:ident $v:ident) => {{ let () = $v; Request::$variant }};
    (@wrap $variant:ident $v:ident $p:ty) => { Request::$variant($v) };
}

methods! {
    Hello = "hello" (HelloParams) -> HelloResult;
    Subscribe = "subscribe" (SubscribeParams) -> SubscribeResult;
    WriteNote = "write_note" (WriteNoteParams) -> WriteNoteResult;
    CreateNote = "create_note" (CreateNoteParams) -> CreateNoteResult;
    WriteAttachment = "write_attachment" (WriteAttachmentParams) -> WriteAttachmentResult;
    Rename = "rename" (RenameParams) -> RenameResult;
    Delete = "delete" (DeleteParams) -> DeleteResult;
    DeletePreview = "delete_preview" (PathParams) -> DeletePreviewResult;
    DeleteNote = "delete_note" (DeleteNoteParams) -> DeleteNoteResult;
    ActionsList = "actions_list" -> Vec<ActionInfo>;
    RunAction = "run_action" (RunActionParams) -> RunActionResult;
    Job = "job" (JobIdParams) -> JobStatus;
    Cancel = "cancel" (JobIdParams) -> JobStatus;
    JobsList = "jobs_list" (JobsListParams) -> JobsListResult;
    JobRetry = "job_retry" (JobIdParams) -> JobStatus;
    IntakeList = "intake_list" (IntakeListParams) -> IntakeListResult;
    Uri = "uri" (UriParams) -> UriResult;
    Status = "status" -> StatusResult;
    ViewsList = "views_list" -> ViewsListResult;
    ViewRun = "view_run" (ViewRunParams) -> ViewResult;
    LookupCurrencies = "lookup_currencies" -> Vec<LookupCurrency>;
    LookupGeocode = "lookup_geocode" (LookupGeocodeParams) -> Vec<LookupGeoResult>;
    LookupModels = "lookup_models" (LookupModelsParams) -> LookupModelsResult;
    LookupChannels = "lookup_channels" (LookupChannelsParams) -> Vec<LookupChannel>;
    Search = "search" (SearchParams) -> SearchResult;
    TrackerItemSetStatus = "tracker_item_set_status" (TrackerItemSetStatusParams) -> ();
    NoteSetBool = "note_set_bool" (NoteSetBoolParams) -> NoteSetBoolResult;
}
