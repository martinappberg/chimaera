fn main() -> anyhow::Result<()> {
    let deck = r#"{
      "format": "chimaera.board", "formatVersion": 1,
      "title": "t",
      "canvas": { "size": [960, 540] },
      "pages": [{
        "id": "p1",
        "objects": [
          { "id": "title", "type": "text", "role": "title", "at": [72, 56], "size": [816, 56],
            "text": ["Parse time drops on every fixture"] },
          { "id": "chart", "type": "chart", "at": [72, 152], "size": [480, 300],
            "data": { "origin": "command", "values": [
              {"f": "large.json", "ms": 812, "err": 24, "build": "before"},
              {"f": "large.json", "ms": 244, "err": 11, "build": "after"},
              {"f": "small.json", "ms": 91, "err": 6, "build": "before"},
              {"f": "small.json", "ms": 30, "err": 3, "build": "after"},
              {"f": "nested.json", "ms": 407, "err": 18, "build": "before"},
              {"f": "nested.json", "ms": 120, "err": 9, "build": "after"}]},
            "x": {"field": "f"}, "y": {"field": "ms", "title": "Parse time (ms)"},
            "color": {"field": "build"},
            "marks": [{"mark": "bar", "stack": "group"},
                      {"mark": "errorbar", "fields": {"err": "err"}}] },
          { "id": "callout", "type": "shape", "geo": "roundRect", "at": [616, 208], "size": [272, 96],
            "fill": "@surface", "stroke": {"color": "@accent1", "width": 1.5},
            "text": [{"runs": [{"t": "3.3× median", "b": true}]}] },
          { "id": "arrow", "type": "connector", "geo": "straight",
            "from": {"object": "callout", "side": "left"},
            "to": {"object": "chart", "side": "right"},
            "stroke": {"color": "@fg", "width": 1.5}, "tailEnd": "arrow" }
        ]
      }]
    }"#;
    for (theme_id, dark) in [("talk-dark", true), ("talk-light", false)] {
        let mut b = chimaera_board::parse(deck)?;
        chimaera_board::normalize(&mut b);
        let theme = chimaera_board::theme::default_for(dark);
        let fonts = chimaera_board::layout::FontStack::new(&[]);
        let out = chimaera_board::render::render_page(&b, 0, &theme, &fonts, Default::default())?;
        std::fs::write(format!("/tmp/board-{theme_id}.png"), &out.png)?;
        for d in &out.diagnostics {
            eprintln!("{theme_id}: {}", d.render());
        }
    }
    // The show path too.
    let spec: chimaera_board::show::ShowSpec = serde_json::from_str(
        r#"{
      "title": "Test failures by file", "note": "after the parser rewrite; 3 runs",
      "chart": {"x": "file", "y": "failures", "values": [
        {"file": "parser.rs", "failures": 12}, {"file": "lexer.rs", "failures": 3},
        {"file": "ast.rs", "failures": 1}, {"file": "codegen.rs", "failures": 7}]}}"#,
    )?;
    let board = chimaera_board::show::build_board(&spec, [720.0, 450.0], "talk-dark")?;
    let theme = chimaera_board::theme::default_for(true);
    let fonts = chimaera_board::layout::FontStack::new(&[]);
    let out = chimaera_board::render::render_page(&board, 0, &theme, &fonts, Default::default())?;
    std::fs::write("/tmp/board-shown.png", &out.png)?;
    println!("ok");
    Ok(())
}
