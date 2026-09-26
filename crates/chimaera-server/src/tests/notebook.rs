use super::support::*;
use crate::*;

fn write_notebook(dir: &std::path::Path, name: &str, cells: Vec<serde_json::Value>) -> String {
    let nb = serde_json::json!({
        "cells": cells,
        "metadata": {
            "kernelspec": {"name": "python3", "language": "python", "display_name": "Python 3"},
            "widgets": {"application/vnd.jupyter.widget-state+json": {"state": {"x": [1, 2, 3]}}}
        },
        "nbformat": 4,
        "nbformat_minor": 5
    });
    let path = dir.join(name);
    std::fs::write(&path, serde_json::to_vec(&nb).unwrap()).unwrap();
    path.to_string_lossy().into_owned()
}

fn code_cell(n: i64, outputs: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({
        "cell_type": "code",
        "execution_count": n,
        "metadata": {},
        "source": [format!("x = {n}\n"), "print(x)"],
        "outputs": outputs
    })
}

async fn get_notebook(state: &Arc<AppState>, path: &str, query: &str) -> serde_json::Value {
    let (status, json) = request(
        state,
        Method::GET,
        &format!("/api/v1/fs/notebook?path={}{query}", urlencode(path)),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    json
}

#[tokio::test]
async fn fs_notebook_pages_cells_and_normalizes_outputs() {
    let state = test_state();
    let dir = test_dir("notebook-page");
    let mut cells = vec![serde_json::json!({
        "cell_type": "markdown",
        "metadata": {},
        "source": ["# Title\n", "Some $x^2$ math."],
        "attachments": {"a.png": {"image/png": "iVBORw0KGgo=", "text/plain": "img"}}
    })];
    cells.push(code_cell(
        1,
        vec![
            serde_json::json!({"output_type": "stream", "name": "stdout", "text": ["1\n", "2\n"]}),
            serde_json::json!({
                "output_type": "execute_result",
                "execution_count": 1,
                "metadata": {},
                "data": {
                    "text/plain": ["<Figure>"],
                    "image/png": "iVBORw0KGgo=",
                    "text/html": "<b>also</b>",
                    "application/vnd.plotly.v1+json": {"data": [1, 2, 3]}
                }
            }),
            serde_json::json!({
                "output_type": "error",
                "ename": "ValueError",
                "evalue": "bad",
                "traceback": ["\u{1b}[31mTraceback\u{1b}[0m", "ValueError: bad"]
            }),
        ],
    ));
    for n in 2..=60 {
        cells.push(code_cell(n, vec![]));
    }
    let path = write_notebook(&dir, "nb.ipynb", cells);

    let page = get_notebook(&state, &path, "&offset=0&limit=5").await;
    assert_eq!(page["total"], 61);
    assert_eq!(page["offset"], 0);
    assert_eq!(page["nbformat"], 4);
    assert_eq!(page["language"], "python");
    let got = page["cells"].as_array().unwrap();
    assert_eq!(got.len(), 5);

    let md = &got[0];
    assert_eq!(md["cell_type"], "markdown");
    assert_eq!(md["source"], "# Title\nSome $x^2$ math.");
    assert_eq!(
        md["attachments"]["a.png"]["data"]["image/png"],
        "iVBORw0KGgo="
    );

    let code = &got[1];
    assert_eq!(code["index"], 1);
    assert_eq!(code["execution_count"], 1);
    assert_eq!(code["source"], "x = 1\nprint(x)");
    let outs = code["outputs"].as_array().unwrap();
    assert_eq!(outs[0]["output_type"], "stream");
    assert_eq!(outs[0]["text"], "1\n2\n");
    // The richest drawable mime plus the text fallback; html and vendor JSON
    // never cross the wire when a png is there.
    let data = outs[1]["data"].as_object().unwrap();
    assert_eq!(data.len(), 2, "{data:?}");
    assert_eq!(data["image/png"], "iVBORw0KGgo=");
    assert_eq!(data["text/plain"], "<Figure>");
    assert_eq!(outs[1]["execution_count"], 1);
    assert_eq!(outs[2]["ename"], "ValueError");
    assert_eq!(
        outs[2]["traceback"],
        "\u{1b}[31mTraceback\u{1b}[0m\nValueError: bad"
    );

    // A later page starts where the offset says and knows the total.
    let page = get_notebook(&state, &path, "&offset=58&limit=10").await;
    let got = page["cells"].as_array().unwrap();
    assert_eq!(got.len(), 3);
    assert_eq!(got[0]["index"], 58);
    assert_eq!(page["total"], 61);

    // The limit is clamped to 100 per page.
    let page = get_notebook(&state, &path, "&limit=100000").await;
    assert_eq!(page["cells"].as_array().unwrap().len(), 61);
    let many: Vec<_> = (0..150).map(|n| code_cell(n, vec![])).collect();
    let big = write_notebook(&dir, "many.ipynb", many);
    let page = get_notebook(&state, &big, "&limit=100000").await;
    assert_eq!(page["cells"].as_array().unwrap().len(), 100);
    assert_eq!(page["total"], 150);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn fs_notebook_caps_payloads_text_and_page_size() {
    let state = test_state();
    let dir = test_dir("notebook-caps");
    let huge_png = "A".repeat(9 * 1024 * 1024);
    let long_text = "y".repeat(300 * 1024);
    let cells = vec![
        code_cell(
            1,
            vec![serde_json::json!({
                "output_type": "display_data",
                "metadata": {},
                "data": {"image/png": huge_png, "text/plain": "<Figure size 640x480>"}
            })],
        ),
        code_cell(
            2,
            vec![serde_json::json!({"output_type": "stream", "name": "stderr", "text": long_text})],
        ),
    ];
    let path = write_notebook(&dir, "caps.ipynb", cells);
    let page = get_notebook(&state, &path, "").await;
    let cells = page["cells"].as_array().unwrap();

    // Over the 8 MB payload cap: a placeholder with the size, never the bytes.
    let out = &cells[0]["outputs"][0];
    assert!(out["data"].get("image/png").is_none());
    assert_eq!(out["omitted"]["image/png"], 9 * 1024 * 1024);
    assert_eq!(out["data"]["text/plain"], "<Figure size 640x480>");

    // Over the 200 KB text cap: cut and flagged.
    let out = &cells[1]["outputs"][0];
    assert_eq!(out["truncated"], true);
    assert_eq!(out["text"].as_str().unwrap().len(), 200 * 1024);

    // A page stops at its payload budget (always carrying one cell); the
    // client continues from offset + cells.len().
    let img = "B".repeat(6 * 1024 * 1024);
    let figs: Vec<_> = (0..5)
        .map(|n| {
            code_cell(
                n,
                vec![serde_json::json!({
                    "output_type": "display_data",
                    "metadata": {},
                    "data": {"image/png": img}
                })],
            )
        })
        .collect();
    let path = write_notebook(&dir, "figs.ipynb", figs);
    let page = get_notebook(&state, &path, "&limit=5").await;
    assert_eq!(page["cells"].as_array().unwrap().len(), 3);
    assert_eq!(page["total"], 5);
    let page = get_notebook(&state, &path, "&offset=3&limit=5").await;
    assert_eq!(page["cells"].as_array().unwrap().len(), 2);
    assert_eq!(page["cells"][0]["index"], 3);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn fs_notebook_rejects_oversize_old_and_malformed_notebooks() {
    let state = test_state();
    let dir = test_dir("notebook-reject");

    // Over the 64 MB source cap (sparse: nothing is actually written).
    let big = dir.join("big.ipynb");
    std::fs::File::create(&big)
        .unwrap()
        .set_len(65 * 1024 * 1024)
        .unwrap();
    // nbformat 3 keeps its cells under worksheets.
    let old = dir.join("old.ipynb");
    std::fs::write(
        &old,
        r#"{"worksheets":[{"cells":[]}],"metadata":{},"nbformat":3,"nbformat_minor":0}"#,
    )
    .unwrap();
    let broken = dir.join("broken.ipynb");
    std::fs::write(&broken, r#"{"cells": [ {"cell_type": "code""#).unwrap();

    for (path, needle) in [
        (&big, "preview cap"),
        (&old, "nbformat 3"),
        (&broken, "not a readable notebook"),
        (&dir, "not a file"),
    ] {
        let (status, json) = request(
            &state,
            Method::GET,
            &format!(
                "/api/v1/fs/notebook?path={}",
                urlencode(&path.to_string_lossy())
            ),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        assert!(
            json["error"].as_str().unwrap().contains(needle),
            "{needle}: {json}"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn fs_notebook_requires_the_bearer_token() {
    let res = app(test_state())
        .oneshot(
            Request::builder()
                .uri("/api/v1/fs/notebook?path=/tmp/x.ipynb")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
