pub mod impls;
pub mod loader;

use anyhow::Result;

//TODO function load, run, cancel to async
trait PluginRunner: Send + Sync {
    fn name(&self) -> String;
    fn load(&mut self, operation: Vec<u8>) -> Result<()>;
    fn run(&mut self, arg: Vec<u8>) -> Result<Vec<Vec<u8>>>;
    fn cancel(&mut self) -> bool;
    fn operation_proto(&self) -> String;
    fn job_args_proto(&self) -> String;
    fn result_output_proto(&self) -> Option<String>;
    fn use_job_result(&self) -> bool;
}
