use app_wrapper::llm::skills::SkillCatalog;
use jobworkerp_runner::jobworkerp::runner::llm::SkillSettings;
use jobworkerp_runner::jobworkerp::runner::{CommandArgs, CommandResult};
use jobworkerp_runner::runner::RunnerTrait;
use jobworkerp_runner::runner::command::CommandRunnerImpl;
use prost::Message;
use serde_json::Value;
use std::fs;
use std::sync::{Mutex, MutexGuard};
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct SkillRootsEnv {
    previous: Option<std::ffi::OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl SkillRootsEnv {
    fn set(json: &str) -> Self {
        let lock = ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("LLM_SKILL_ROOTS");
        unsafe { std::env::set_var("LLM_SKILL_ROOTS", json) };
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for SkillRootsEnv {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            unsafe { std::env::set_var("LLM_SKILL_ROOTS", previous) };
        } else {
            unsafe { std::env::remove_var("LLM_SKILL_ROOTS") };
        }
    }
}

#[test]
fn public_catalog_api_keeps_block_scalar_text_out_of_yaml_alias_detection() {
    let temp = TempDir::new().unwrap();
    let local_root = temp.path().join("local");
    let resource_root = temp.path().join("read-only-map");
    let skill_dir = local_root.join("literal-block");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::create_dir_all(&resource_root).unwrap();
    let original = "---\nname: literal-block\ndescription: | # YAML block scalar\n  Literal *alias &anchor !tag and <XML>.\n---\nfull skill instructions\n";
    fs::write(skill_dir.join("SKILL.md"), original).unwrap();

    let roots = serde_json::json!({
        "team": {
            "local_path": local_root.to_string_lossy(),
            "resource_path": resource_root.to_string_lossy(),
        }
    })
    .to_string();
    let _env = SkillRootsEnv::set(&roots);
    let settings = SkillSettings {
        root_ids: vec!["team".to_string()],
        allow_names: Vec::new(),
    };

    let catalog = SkillCatalog::load_shared(&settings).unwrap();
    assert_eq!(catalog.names(), ["literal-block"]);
    assert!(
        catalog
            .render_prompt()
            .unwrap()
            .contains("Literal *alias &amp;anchor !tag and &lt;XML&gt;.")
    );
    let schema = catalog.activation_tool_schema().unwrap();
    assert_eq!(
        schema["parameters"]["properties"]["name"]["enum"],
        serde_json::json!(["literal-block"])
    );

    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: literal-block\ndescription: changed\n---\nnew instructions\n",
    )
    .unwrap();
    let activation: Value =
        serde_json::from_str(&catalog.activate_json(r#"{"name":"literal-block"}"#)).unwrap();
    assert_eq!(
        activation["resource_base_dir"],
        serde_json::json!(resource_root.join("literal-block").to_string_lossy())
    );
    assert_eq!(activation["content"], original);
}

#[tokio::test]
async fn activated_resource_path_can_be_read_by_a_separate_command_tool() -> anyhow::Result<()> {
    let temp = TempDir::new().unwrap();
    let local = temp.path().join("llm-node");
    let resource = temp.path().join("read-node");
    for root in [&local, &resource] {
        let dir = root.join("guide");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("references.txt"), "reference-only material").unwrap();
    }
    fs::write(
        local.join("guide/SKILL.md"),
        "---\nname: guide\ndescription: Read external references on demand\n---\nInstructions without reference text",
    )
    .unwrap();
    let roots = serde_json::json!({"team": {
        "local_path": local.to_string_lossy(),
        "resource_path": resource.to_string_lossy(),
    }})
    .to_string();
    let _env = SkillRootsEnv::set(&roots);
    let catalog = SkillCatalog::load(&SkillSettings {
        root_ids: vec!["team".into()],
        allow_names: vec![],
    })?;
    assert!(
        !catalog
            .render_prompt()
            .unwrap()
            .contains("reference-only material")
    );
    let activation: Value = serde_json::from_str(&catalog.activate_json(r#"{"name":"guide"}"#))?;
    let reference = std::path::Path::new(activation["resource_base_dir"].as_str().unwrap())
        .join("references.txt");
    assert!(reference.starts_with(&resource));
    assert!(!reference.starts_with(&local));
    let mut command = CommandRunnerImpl::new();
    command.load(vec![]).await?;
    let read = |path: &std::path::Path| CommandArgs {
        command: "cat".into(),
        args: vec![path.to_string_lossy().into_owned()],
        treat_nonzero_as_error: true,
        ..Default::default()
    };
    let (bytes, _) = command
        .run(&read(&reference).encode_to_vec(), Default::default(), None)
        .await;
    assert_eq!(
        CommandResult::decode(bytes?.as_slice())?.stdout.as_deref(),
        Some("reference-only material")
    );
    fs::remove_file(&reference)?;
    assert!(
        command
            .run(&read(&reference).encode_to_vec(), Default::default(), None)
            .await
            .0
            .is_err()
    );
    Ok(())
}
