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
}
