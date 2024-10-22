use crate::infra::runner::Runner;
use std::path::Path;
use std::sync::{Arc, RwLock};

use super::PluginLoader;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use command_utils::util::result::{Flatten, TapErr as _, ToOption as _};
use libloading::{Library, Symbol};

//TODO function load, run, cancel to async
pub trait PluginRunner: Send + Sync {
    fn name(&self) -> String;
    fn load(&mut self, operation: Vec<u8>) -> Result<()>;
    fn run(&mut self, arg: Vec<u8>) -> Result<Vec<Vec<u8>>>;
    fn cancel(&mut self) -> bool;
    fn operation_proto(&self) -> String;
    fn job_args_proto(&self) -> String;
    fn use_job_result(&self) -> bool;
}

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
        if let Some((name, _, lib)) = self.plugin_loaders.iter().find(|p| p.0.as_str() == name) {
            // XXX unsafe
            unsafe { lib.get(b"load_plugin") }
                .tap_err(|e| tracing::error!("error in loading runner plugin:{name}, error: {e:?}"))
                .to_option()
                .map(|lp: LoaderFunc<'_>| PluginRunnerWrapperImpl::new(Arc::new(RwLock::new(lp()))))
        } else {
            None
        }
    }

    // // if operation is Some, load plugin with operation
    // // (blocking)
    // pub async fn load_runner_by_name<'a>(
    //     &self,
    //     name: &'a str,
    //     operation: Vec<u8>,
    // ) -> Result<Box<dyn Runner + Send + Sync>> {
    //     if let Some(p) = self.find_plugin_runner_by_name(name) {
    //         let p = p.create(operation).await?;
    //         Ok(Box::new(p) as Box<dyn Runner + Send + Sync>)
    //     } else {
    //         Err(anyhow!("plugin not found: {}", name))
    //     }
    // }
}

impl Default for RunnerPluginLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginLoader for RunnerPluginLoader {
    // TODO
    fn load_path(&mut self, path: &Path) -> Result<String> {
        // XXX load plugin only for getting name
        let lib = unsafe { Library::new(path) }?;
        let load_plugin: LoaderFunc = unsafe { lib.get(b"load_plugin") }?;
        let plugin = load_plugin();
        let name = plugin.name().clone();

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
            Ok(name)
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

/**
 * PluginRunner wrapper
 * (for self mutability (run(), cancel()))
 */
#[derive(Clone)]
pub struct PluginRunnerWrapperImpl {
    #[allow(clippy::borrowed_box)]
    plugin_runner: Arc<RwLock<Box<dyn PluginRunner + Send + Sync>>>,
}

impl PluginRunnerWrapperImpl {
    // #[allow(clippy::borrowed_box)]
    pub fn new(plugin_runner: Arc<RwLock<Box<dyn PluginRunner + Send + Sync>>>) -> Self {
        Self { plugin_runner }
    }
    pub async fn create(&self, operation: Vec<u8>) -> Result<()> {
        let plugin_runner = Arc::clone(&self.plugin_runner);
        #[allow(unstable_name_collisions)]
        tokio::task::spawn_blocking(move || {
            plugin_runner
                .write()
                .map_err(|e| anyhow!("plugin runner lock error: {:?}", e))
                .and_then(|mut r| r.load(operation))
        })
        .await
        .map_err(|e| e.into())
        .flatten()?;
        Ok(())
    }
}

#[async_trait]
impl Runner for PluginRunnerWrapperImpl {
    fn name(&self) -> String {
        let plugin_runner = Arc::clone(&self.plugin_runner);
        let n = plugin_runner
            .read()
            .map(|p| p.name())
            .unwrap_or_else(|e| format!("Error occurred: {:}", e));
        n
    }
    async fn load(&mut self, operation: Vec<u8>) -> Result<()> {
        self.create(operation).await?;
        Ok(())
    }
    // arg: assumed as utf-8 string, specify multiple arguments with \n separated
    #[allow(unstable_name_collisions)]
    async fn run(&mut self, arg: &[u8]) -> Result<Vec<Vec<u8>>> {
        // XXX clone
        let plugin_runner = self.plugin_runner.clone();
        let arg1 = arg.to_vec();
        tokio::task::spawn_blocking(move || {
            match plugin_runner
                .write()
                .map_err(|e| anyhow::anyhow!("plugin runner lock error: {:?}", e))
            {
                Ok(mut runner) => runner.run(arg1).map_err(|e| {
                    tracing::warn!("in running pluginRunner: {:?}", e);
                    anyhow!("in running pluginRunner: {:?}", e)
                }),
                Err(e) => Err(e),
            }
        })
        .await
        .map_err(|e| e.into())
        .flatten()
    }

    async fn cancel(&mut self) {
        let _ = self.plugin_runner.write().map(|mut r| r.cancel());
    }
    fn operation_proto(&self) -> String {
        let plugin_runner = Arc::clone(&self.plugin_runner);
        plugin_runner
            .read()
            .map(|p| p.operation_proto())
            .unwrap_or_else(|e| format!("Error occurred: {:}", e))
    }
    fn job_args_proto(&self) -> String {
        let plugin_runner = Arc::clone(&self.plugin_runner);
        plugin_runner
            .read()
            .map(|p| p.job_args_proto())
            .unwrap_or_else(|e| format!("Error occurred: {:}", e))
    }
    fn use_job_result(&self) -> bool {
        let plugin_runner = Arc::clone(&self.plugin_runner);
        plugin_runner
            .read()
            .map(|p| p.use_job_result())
            .unwrap_or_else(|e| {
                tracing::warn!("Error occurred: {:}", e);
                false
            })
    }
}
