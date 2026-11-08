//! Acceptance tests for docs/10 §9: scripting, rollback and bundled plugins.

#[cfg(test)]
mod acceptance {
    use super::super::*;
    use crate::document::Document;
    fn engine() -> Engine {
        Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()))
    }
    #[test]
    fn script_commands_queries_and_units_are_one_undo_step() {
        let mut e = engine();
        let r = eval(
            &mut e,
            r#"
            group("Box", || {
                rectangle(#{corner_a:pt(0,0,0),corner_b:pt(m(1),cm(50),0)});
                push_pull(#{face:sel("face & last"),distance:mm(250)});
            });
            let faces=query("face");
            select(faces,"replace");
            print(faces.len());
            faces.len()
        "#,
            &json!({}),
            Default::default(),
            Default::default(),
        )
        .unwrap();
        assert_eq!(r.value, json!(6));
        assert_eq!(r.output, vec!["6"]);
        assert_eq!(e.history().applied(), 1);
        assert_eq!(e.selection().len(), 6);
        assert!(e.document().model.mesh().is_solid());
        e.undo().unwrap();
        assert!(e.document().model.is_empty());
        e.redo().unwrap();
        assert!(e.document().model.mesh().is_solid());
    }
    #[test]
    fn failed_and_runaway_scripts_do_not_commit() {
        let mut e = engine();
        for source in [
            r#"rectangle(#{corner_a:pt(0,0,0),corner_b:pt(100,100,0)}); throw "stop";"#,
            "loop {}",
            "eval(\"1\")",
            "import \"secret\" as secret;",
        ] {
            let result = eval(
                &mut e,
                source,
                &json!({}),
                ScriptLimits {
                    operations: 1000,
                    ..Default::default()
                },
                Default::default(),
            );
            assert!(result.is_err(), "{source}");
            assert!(e.document().model.is_empty());
            assert_eq!(e.history().applied(), 0);
        }
    }
    #[test]
    fn all_three_bundled_tools_execute_and_undo() {
        let mut registry = CommandRegistry::with_builtins();
        let plugins = plugins::bundled().unwrap();
        assert_eq!(plugins.len(), 3);
        for plugin in &plugins {
            plugin.register(&mut registry).unwrap();
        }
        for name in [
            "ext:org.planyrith.stairs/stairs",
            "ext:org.planyrith.cabinet/cabinet",
            "ext:org.planyrith.csv-points/points",
        ] {
            let mut e = Engine::new(
                Document::new(),
                Arc::new({
                    let mut r = CommandRegistry::with_builtins();
                    for p in &plugins {
                        p.register(&mut r).unwrap();
                    }
                    r
                }),
            );
            e.execute_erased(name, json!({}))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!e.document().model.is_empty());
            assert!(crate::diagnostics::validate(e.document()).is_clean());
            e.undo().unwrap();
            assert!(e.document().model.is_empty());
            e.redo().unwrap();
        }
    }

    /// A native plugin (docs/10 §7) in the text format: a tool that draws a
    /// rectangle through the host and logs, a menu callback, a callback that
    /// never ends and one that asks for a forbidden command.
    fn wasm_plugin() -> plugins::Plugin {
        let wat = r#"
(module
  (import "planyrith" "call" (func $call (param i32 i32 i32 i32) (result i64)))
  (import "planyrith" "query" (func $query (param i32 i32) (result i64)))
  (import "planyrith" "log" (func $log (param i32 i32)))
  (memory (export "memory") 1)
  (global $next (mut i32) (i32.const 4096))
  (data (i32.const 0) "rectangle")
  (data (i32.const 16) "{\"corner_a\":[0,0,0],\"corner_b\":[1000,500,0]}")
  (data (i32.const 128) "drawn")
  (data (i32.const 144) "face")
  (data (i32.const 160) "script-eval")
  (data (i32.const 176) "{}")
  (func (export "planyrith_alloc") (param $size i32) (result i32)
    (local $at i32)
    (local.set $at (global.get $next))
    (global.set $next (i32.add (global.get $next) (local.get $size)))
    (local.get $at))
  (func (export "build") (param i32 i32) (result i64)
    (drop (call $call (i32.const 0) (i32.const 9) (i32.const 16) (i32.const 44)))
    (call $log (i32.const 128) (i32.const 5))
    (call $query (i32.const 144) (i32.const 4)))
  (func (export "hello") (param i32 i32) (result i64)
    (call $log (i32.const 128) (i32.const 5))
    (i64.const 0))
  (func (export "spin") (param i32 i32) (result i64)
    (loop $forever (br $forever))
    (i64.const 0))
  (func (export "sneak") (param i32 i32) (result i64)
    (call $call (i32.const 160) (i32.const 11) (i32.const 176) (i32.const 2))))
"#;
        let manifest = r#"
[plugin]
id = "org.example.native"
name = "Native"
version = "1.0.0"
license = "MIT"
min_planyrith = "0.8.0"

[entry]
wasm = "plugin.wasm"

[[tools]]
name = "slab"
label = "Slab"
params_schema = "schema.json"
on_execute = "build"

[[tools]]
name = "spin"
label = "Spin"
params_schema = "schema.json"
on_execute = "spin"

[[tools]]
name = "sneak"
label = "Sneak"
params_schema = "schema.json"
on_execute = "sneak"

[[menus]]
path = "Extensions/Native/Hello"
on_click = "hello"
"#;
        let mut files = std::collections::BTreeMap::new();
        files.insert("plugin.toml".to_string(), manifest.as_bytes().to_vec());
        files.insert("plugin.wasm".to_string(), wat::parse_str(wat).unwrap());
        files.insert(
            "schema.json".to_string(),
            br#"{"type":"object","properties":{}}"#.to_vec(),
        );
        plugins::Plugin::from_files(files).unwrap()
    }

    #[test]
    fn native_plugins_call_the_registry_in_one_undo_step() {
        let plugin = wasm_plugin();
        let mut registry = CommandRegistry::with_builtins();
        plugin.register(&mut registry).unwrap();
        assert_eq!(
            plugin.callback("hello").unwrap(),
            plugins::Callback::Command("ext:org.example.native/hello".into())
        );
        let mut e = Engine::new(Document::new(), Arc::new(registry));
        let out = e
            .execute_erased("ext:org.example.native/slab", json!({}))
            .unwrap();
        // The query after the call sees the rectangle's face.
        assert_eq!(out["ok"].as_array().map(Vec::len), Some(1), "{out}");
        assert_eq!(e.document().model.mesh().faces().count(), 1);
        assert_eq!(e.history().applied(), 1);
        e.undo().unwrap();
        assert!(e.document().model.is_empty());
        e.execute_erased("ext:org.example.native/hello", json!({}))
            .unwrap();

        // A runaway callback stops at the budget; a forbidden call is
        // answered with an error; neither changes the document.
        let err = e
            .execute_erased("ext:org.example.native/spin", json!({}))
            .unwrap_err();
        assert!(err.to_string().contains("budget"), "{err}");
        let out = e
            .execute_erased("ext:org.example.native/sneak", json!({}))
            .unwrap();
        assert!(
            out["error"]["message"]
                .as_str()
                .unwrap()
                .contains("cannot be called")
        );
        assert!(e.document().model.is_empty());

        // Modules with unknown imports or missing callbacks are refused.
        let mut files = std::collections::BTreeMap::new();
        files.insert(
            "plugin.wasm".to_string(),
            wat::parse_str(r#"(module (import "env" "open" (func)) (memory (export "memory") 1))"#)
                .unwrap(),
        );
        assert!(super::super::wasm::check(&files["plugin.wasm"], &[]).is_err());
        let module = wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "planyrith_alloc") (param i32) (result i32) (i32.const 0)))"#).unwrap();
        assert!(super::super::wasm::check(&module, &[]).is_ok());
        assert!(super::super::wasm::check(&module, &["build"]).is_err());
    }
}
