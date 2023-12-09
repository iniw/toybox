use defmt::*;
use embassy_executor::*;
use embassy_time::*;
use heapless::*;

use crate::stations;

pub mod controller;
pub mod executor;

#[derive(Format, Clone, Copy)]
pub struct Step {
    pub duration: Duration,
    pub interval: Option<Duration>,
}

#[derive(Format, Clone)]
pub struct Recipe {
    pub scald: Option<Step>,
    pub attacks: RecipeSteps,
    pub finalization_time: Option<Duration>,
}

impl Recipe {
    pub fn has_scald(&self) -> bool {
        return self.scald.is_some();
    }
}

pub type RecipeSteps = Vec<Step, MAX_RECIPE_STEPS>;

const MAX_RECIPE_STEPS: usize = 10;

const MAX_EVENTS_PER_STATION: usize = 5;

pub(super) fn spawn_tasks(spawner: &Spawner, ctx: &'static crate::SharedContext) {
    unwrap!(spawner.spawn(executor::main_task(executor::Context {
        num_steps_per_station: stations::make_per_station_data!(0usize),
        step_queue: executor::StepQueue::new(),
        recipe_executor_channel: &ctx.recipe_executor_channel,
        recipe_controller_channels: &ctx.recipe_controller_channels,
    })));

    let default_recipe = Some(Recipe {
        scald: Some(Step {
            duration: Duration::from_secs(5),
            interval: None,
        }),
        attacks: Vec::from_slice(&[
            Step {
                duration: Duration::from_secs(10),
                interval: Some(Duration::from_secs(5)),
            },
            Step {
                duration: Duration::from_secs(5),
                interval: None,
            },
        ])
        .unwrap(),
        finalization_time: Some(Duration::from_secs(15)),
    });

    for i in 0..stations::MAX_STATIONS {
        unwrap!(spawner.spawn(controller::main_task(controller::Context {
            station_status: stations::Status::Free,
            fixed_recipe: None,
            recipe: default_recipe.clone(),
            station: stations::Station(i),
            recipe_executor_channel: &ctx.recipe_executor_channel,
            recipe_controller_channel: ctx.recipe_controller_channels[i],
            station_status_signal: ctx.station_status_signals[i],
        })));
    }
}
