use super::PluginLoader;
use crate::runner::plugins::{impls::PluginRunnerWrapperImpl, PluginRunner};
use anyhow::{anyhow, Result};
use command_utils::util::result::ToOption as _;
use libloading::{Library, Symbol};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

#[allow(improper_ctypes_definitions)]
type LoaderFunc<'a> = Symbol<'a, extern "C" fn() -> Box<dyn PluginRunner + Send + Sync>>;

#[derive(Debug)]
pub struct RunnerPluginLoader {
    // TODO to map?
    plugin_loaders: Vec<(String, String, Library)>,
}

impl RunnerPluginLoader {
    pub fn new() -> Self {
        RunnerPluginLoader {
            plugin_loaders: Vec::new(),
        }
    }
    pub fn plugin_loaders(&self) -> &Vec<(String, String, Library)> {
        &self.plugin_loaders
    }
    pub fn exists(&self, name: &str) -> bool {
        self.plugin_loaders.iter().any(|p| p.0.as_str() == name)
    }

    // find plugin (not loaded. reference only. cannot run)
    pub fn find_plugin_runner_by_name(&self, name: &str) -> Option<PluginRunnerWrapperImpl> {
        if let Some((_name, _, lib)) = self.plugin_loaders.iter().find(|p| p.0.as_str() == name) {
            // XXX unsafe
            unsafe { lib.get(b"load_plugin") }
                .to_option()
                .map(|lp: LoaderFunc<'_>| PluginRunnerWrapperImpl::new(Arc::new(RwLock::new(lp()))))
        } else {
            None
        }
    }
}

impl Default for RunnerPluginLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginLoader for RunnerPluginLoader {
    fn load_path(&mut self, path: &Path) -> Result<(String, String)> {
        // XXX load plugin only for getting name
        let lib = unsafe { Library::new(path) }?;
        let load_plugin: LoaderFunc = unsafe { lib.get(b"load_plugin") }?;
        let plugin = load_plugin();
        let name = plugin.name().clone();
        let description = plugin.description().clone();

        let lib = unsafe { Library::new(path) }?;
        if self.plugin_loaders.iter().any(|p| p.0.as_str() == name) {
            Err(anyhow!(
                "plugin already loaded: {} ({})",
                name,
                path.display()
            ))
        } else {
            self.plugin_loaders.push((
                name.clone(),
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
                lib,
            ));
            Ok((name, description))
        }
    }
    fn unload(&mut self, name: &str) -> Result<bool> {
        // XXX unload plugin only for getting name
        let idx = self
            .plugin_loaders
            .iter()
            .rev() // unload latest loaded plugin
            .position(|p| p.0.as_str() == name);
        if let Some(i) = idx {
            self.plugin_loaders.remove(i);
            Ok(true)
        } else {
            Ok(false)
        }
    }
    fn clear(&mut self) -> Result<()> {
        self.plugin_loaders.clear();
        Ok(())
    }
}

impl Drop for RunnerPluginLoader {
    fn drop(&mut self) {
        // Plugin drop must be called before Library drop.
        self.plugin_loaders.clear();
    }
}
