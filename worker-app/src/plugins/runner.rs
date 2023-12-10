use super::PluginLoader;
use crate::worker::runner::{impls::plugin::PluginRunnerWrapperImpl, Runner};
use anyhow::Result;
use command_utils::util::{
    option::FlatMap,
    result::{TapErr, ToOption},
};
use libloading::{Library, Symbol};
use std::{
    path::Path,
    sync::{Arc, RwLock},
};

pub trait PluginRunner: Send + Sync {
    fn name(&self) -> String;
    fn run(&mut self, arg: Vec<u8>) -> Result<Vec<Vec<u8>>>;
    fn cancel(&self) -> bool;
}

#[allow(improper_ctypes_definitions)]
type LoaderFunc<'a> = Symbol<'a, extern "C" fn() -> Box<dyn PluginRunner + Send + Sync>>;

pub struct RunnerPluginLoader {
    plugin_loaders: Vec<(String, Library)>,
}

impl RunnerPluginLoader {
    pub fn new() -> Self {
        RunnerPluginLoader {
            plugin_loaders: Vec::new(),
        }
    }
    pub fn plugin_loaders(&self) -> &Vec<(String, Library)> {
        &self.plugin_loaders
    }
}

impl Default for RunnerPluginLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginLoader for RunnerPluginLoader {
    fn load(&mut self, path: &Path) -> Result<()> {
        // XXX load plugin for getting name
        let lib = unsafe { Library::new(path) }?;
        let load_plugin: LoaderFunc = unsafe { lib.get(b"load_plugin") }?;
        let plugin = load_plugin();
        let name = plugin.name().clone();

        let lib = unsafe { Library::new(path) }?;
        self.plugin_loaders.push((name, lib));
        // self.libraries.push(lib);
        Ok(())
    }
}

impl Drop for RunnerPluginLoader {
    fn drop(&mut self) {
        // Plugin drop must be called before Library drop.
        self.plugin_loaders.clear();
    }
}

pub trait UsePluginRunner: Send + Sync {
    fn runner_plugins(&self) -> &Vec<(String, Library)>;

    #[allow(clippy::borrowed_box)]
    fn find_and_init_plugin_runner_by_name(
        &self,
        name: &str,
    ) -> Option<Box<dyn Runner + Send + Sync>> {
        self.runner_plugins()
            .iter()
            .find(|p| p.0.as_str() == name)
            .flat_map(|r| {
                // XXX unsafe
                unsafe { r.1.get(b"load_plugin") }
                    .tap_err(|e| {
                        tracing::error!("error in loading runner plugin:{name}, error: {e:?}")
                    })
                    .to_option()
                    .map(|lp: LoaderFunc<'_>| {
                        let runner = lp();
                        // non tokio rwlock (for blocking plugin runner thread)
                        Box::new(PluginRunnerWrapperImpl::new(Arc::new(RwLock::new(runner))))
                            as Box<dyn Runner + Send + Sync>
                    })
            })
    }
}
