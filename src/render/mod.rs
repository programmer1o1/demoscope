// Output rendering: the self-contained HTML viewer (`html`) and the JSON
// model it splices in (`json`). The HTML generators are the crate's public
// byte-slice entry points, re-exported here for `main.rs`/`lib.rs`.
pub mod html;
pub mod json;
