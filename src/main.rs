mod app;
mod camera;
mod config;
mod core;
mod input;
mod math;
mod renderer;
mod scene;

use app::App;
use winit::event_loop::{ControlFlow, EventLoop};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let benchmark = app::BenchmarkConfig::from_args(std::env::args().skip(1));
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = match benchmark {
        Ok(Some(config)) => App::with_benchmark(config),
        Ok(None) => App::new(),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}
