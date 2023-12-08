use core::cmp::Ordering;

use defmt::{debug, unwrap, warn, Format};
use embassy_executor::{task, Spawner};
use embassy_futures::select::{select, Either::*};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
use embassy_time::{Duration, Instant, Timer};
use heapless::{String, Vec};
use static_cell::make_static;

use crate::stations::*;
#[derive(Format, Clone, Copy)]
pub struct Step {
    duration: Duration,
    interval: Option<Duration>,
}

#[derive(Format, Clone, Copy)]
pub struct StepExecutionInfo {
    instant: Instant,
    step: Step,
    station: Station,
}

#[derive(Format, Clone)]
pub struct Recipe {
    scald: Option<Step>,
    attacks: RecipeSteps,
    finalization_time: Option<Duration>,
}

impl Recipe {
    const MAX_STEPS: usize = 10;

    pub fn has_scald(&self) -> bool {
        return self.scald.is_some();
    }
}

pub type RecipeSteps = Vec<Step, { Recipe::MAX_STEPS }>;

#[derive(Format)]
pub enum RecipeControllerEvent {
    AdvanceRecipe,
    ButtonPress,
    CancelRecipe,
    #[allow(dead_code)]
    UpdateRecipe(Recipe),
    #[allow(dead_code)]
    UpdateFixedRecipe(Option<Recipe>),
    #[allow(dead_code)]
    InformAboutRecipe(&'static mut String<100>),
}

pub type RecipeControllerChannel = Channel<NoopRawMutex, RecipeControllerEvent, 5>;

#[derive(Format)]
pub enum RecipeExecutorEvent {
    ScheduleAttacks(Station, RecipeSteps),
    ScheduleScald(Station, Step),
    CancelRecipe(Station),
}

pub type RecipeExecutorChannel = Channel<NoopRawMutex, RecipeExecutorEvent, MAX_NUM_STATIONS>;

const TRAVEL_TIME: Duration = Duration::from_secs(1);
type StepQueue = Vec<StepExecutionInfo, { Recipe::MAX_STEPS * MAX_NUM_STATIONS }>;

fn handle_executor_event(ctx: &mut RecipeExecutorContext, event: RecipeExecutorEvent) {
    use RecipeExecutorEvent::*;
    match event {
        ScheduleScald(station, new_step) => {
            ctx.num_steps_per_station[station.0] += 1;

            if ctx.step_queue.is_empty() {
                unwrap!(ctx.step_queue.push(StepExecutionInfo {
                    instant: Instant::now(),
                    step: new_step,
                    station
                }));
            } else {
                for (i, info) in ctx.step_queue.iter().enumerate().rev() {
                    let end = info.instant + info.step.duration;

                    // because `rev` is called after `enumerate` the last iteration is at index 0
                    let at_last_step = i == 0;
                    if at_last_step {
                        let new_step_begin = end + TRAVEL_TIME;
                        unwrap!(ctx.step_queue.insert(
                            0,
                            StepExecutionInfo {
                                instant: new_step_begin,
                                step: new_step,
                                station
                            }
                        ));
                        break;
                    }

                    if !info.step.interval.is_some() {
                        // no way to execute the step if there is no interval between the current + next
                        continue;
                    }

                    let new_step_begin = end + TRAVEL_TIME;
                    let new_step_end = new_step_begin + new_step.duration;

                    // SAFETY: `i` is never 0 because of the `if at_last_step` check above
                    let next_step_begin = ctx.step_queue[i - 1].instant;
                    if new_step_end < next_step_begin - TRAVEL_TIME {
                        unwrap!(ctx.step_queue.insert(
                            i,
                            StepExecutionInfo {
                                instant: new_step_begin,
                                step: new_step,
                                station
                            }
                        ));
                        break;
                    } else {
                        // no luck, move on to the next step
                        continue;
                    }
                }
            }
        }
        ScheduleAttacks(station, steps) => {
            ctx.num_steps_per_station[station.0] += steps.len();

            if ctx.step_queue.is_empty() {
                let _ = steps
                    .iter()
                    .fold(Instant::now() + TRAVEL_TIME, |begin, step| {
                        unwrap!(ctx.step_queue.insert(
                            0,
                            StepExecutionInfo {
                                instant: begin,
                                step: *step,
                                station
                            }
                        ));
                        begin + step.duration + step.interval.unwrap_or(Duration::MIN /* 0 */)
                    });
            } else {
                'outer: for (i, info) in ctx.step_queue.iter().enumerate().rev() {
                    let end = info.instant + info.step.duration;

                    // because `rev` is called after `enumerate` the last iteration is at index 0
                    let at_last_step = i == 0;
                    if at_last_step {
                        let _ = steps.iter().fold(end + TRAVEL_TIME, |begin, step| {
                            unwrap!(ctx.step_queue.insert(
                                0,
                                StepExecutionInfo {
                                    instant: begin,
                                    step: *step,
                                    station
                                }
                            ));
                            begin
                                + step.duration
                                + step.interval.unwrap_or(Duration::MIN /* 0 */)
                        });
                        break;
                    }

                    if !info.step.interval.is_some() {
                        // cant fit in the steps if there is no interval between the current and next step
                        continue;
                    }

                    let next_step_begin = ctx.step_queue[i - 1].instant;

                    let interval_range_begin = (end + TRAVEL_TIME).as_millis();
                    let interval_range_end = (next_step_begin - TRAVEL_TIME).as_millis();
                    const STEP: usize = TRAVEL_TIME.as_millis() as usize;

                    for potential_step_begin in
                        (interval_range_begin..=interval_range_end).step_by(STEP)
                    {
                        let potential_step_begin = Instant::from_millis(potential_step_begin);

                        let mut accumulator = potential_step_begin;
                        let mut queue_steps = steps.iter().map(|step| {
                            let instant = accumulator;
                            accumulator += step.duration + step.interval.unwrap_or(Duration::MIN);
                            StepExecutionInfo {
                                instant,
                                step: *step,
                                station,
                            }
                        });

                        if queue_steps.any(|new_info| {
                            let new_end = new_info.instant + new_info.step.duration;
                            for other_info in ctx.step_queue[0..i].iter().rev() {
                                let end = other_info.instant + other_info.step.duration;
                                if other_info.instant > new_end {
                                    break;
                                }

                                let begin_collides = new_info.instant >= other_info.instant
                                    && new_info.instant <= end;
                                let end_collides = new_end >= other_info.instant && new_end <= end;

                                if begin_collides || end_collides {
                                    return true;
                                }
                            }
                            return false;
                        }) {
                            continue;
                        }

                        // since we have guaranteed that the new steps dont collide with the existing ones
                        // they can just be appended to the queue and sorted (in descending order)
                        ctx.step_queue.extend(queue_steps);
                        ctx.step_queue
                            .sort_unstable_by(|info_a, info_b| info_b.instant.cmp(&info_a.instant));

                        break 'outer;
                    }
                }
            }
        }
        CancelRecipe(canceled_station) => {
            ctx.step_queue
                .retain(|info| info.station != canceled_station);
        }
    }

    debug!("event handled - step queue = {:?}", ctx.step_queue);
}

async fn execute_step(ctx: &RecipeExecutorContext, step: &Step, station: Station) {
    debug!("step begin");
    Timer::after(step.duration).await;
    debug!("step end");

    if ctx.num_steps_per_station[station.0] == 0 {
        ctx.recipe_controller_channels[station.0]
            .send(RecipeControllerEvent::AdvanceRecipe)
            .await;
    }
}

async fn execute_missed_step(
    ctx: &mut RecipeExecutorContext,
    step: &Step,
    station: Station,
    now_as_millis: u64,
    instant_as_millis: u64,
) {
    let delta = now_as_millis - instant_as_millis;
    // this function may get called with `now_as_millis` == `instant_as_millis`, which just makes the loop a waste of time
    if delta > 0 {
        warn!("current step is in the past - delta = {}", delta);

        // compensate by moving all the remaining steps in the queue forward
        for info in ctx.step_queue.iter_mut() {
            info.instant += Duration::from_millis(delta);
        }
    }

    execute_step(ctx, &step, station).await;
}

#[task]
async fn recipe_executor_task(mut ctx: RecipeExecutorContext) {
    loop {
        match ctx.step_queue.is_empty() {
            true => {
                // no steps to execute, just wait for an event
                let event = ctx.recipe_executor_channel.receive().await;
                handle_executor_event(&mut ctx, event);
            }
            false => {
                let info = unwrap!(ctx.step_queue.pop());
                ctx.num_steps_per_station[info.station.0] -= 1;

                // `Instant` has way more precision than we care about.
                // to avoid tripping up the `Ordering::Less` case below it gets converted to milliseconds.
                // in that process it gets rounded down and so it needs to be incremented by 1,
                // otherwise its consistently 1ms in the past
                let instant_as_millis = info.instant.as_millis() + 1;
                let now_as_millis = || Instant::now().as_millis();

                match instant_as_millis.cmp(&now_as_millis()) {
                    Ordering::Less | Ordering::Equal => {
                        execute_missed_step(
                            &mut ctx,
                            &info.step,
                            info.station,
                            now_as_millis(),
                            instant_as_millis,
                        )
                        .await
                    }
                    Ordering::Greater => {
                        // the instant hasn't arrived yet, but we can't just wait for it since we have to keep receiving events
                        // this loop makes us do both at the same time
                        while let Second(event) = select(
                            Timer::at(info.instant),
                            ctx.recipe_executor_channel.receive(),
                        )
                        .await
                        {
                            handle_executor_event(&mut ctx, event);

                            // interpreting the event took long enough for the step instant to be missed
                            if now_as_millis() >= instant_as_millis {
                                execute_missed_step(
                                    &mut ctx,
                                    &info.step,
                                    info.station,
                                    now_as_millis(),
                                    instant_as_millis,
                                )
                                .await;

                                return;
                            }
                        }

                        // falling through the `while` above means that the instant has arrived
                        execute_step(&ctx, &info.step, info.station).await;
                    }
                }
            }
        }
    }
}

fn reset_station(ctx: &mut RecipeControllerContext) {
    ctx.recipe = None;
    ctx.station_status = StationStatus::Free;
    ctx.station_status_signal.signal(ctx.station_status);
}

#[task(pool_size = MAX_NUM_STATIONS)]
async fn recipe_controller_task(mut ctx: RecipeControllerContext) {
    use RecipeControllerEvent::*;
    loop {
        match ctx.recipe_controller_channel.receive().await {
            ButtonPress => match ctx.station_status {
                // try initiating a new recipe
                StationStatus::Free => {
                    // if there is no host-sent recipe try using the fixed one
                    if ctx.recipe.is_none() {
                        ctx.recipe = ctx.fixed_recipe.clone();
                    }

                    if let Some(recipe) = &ctx.recipe {
                        if recipe.has_scald() {
                            ctx.station_status = StationStatus::WaitingToScald;
                            ctx.station_status_signal.signal(ctx.station_status);
                        } else {
                            ctx.station_status = StationStatus::Attacking;
                            ctx.station_status_signal.signal(ctx.station_status);

                            ctx.recipe_executor_channel
                                .send(RecipeExecutorEvent::ScheduleAttacks(
                                    ctx.station,
                                    recipe.attacks.clone(),
                                ))
                                .await;
                        }
                    } else {
                        warn!("no recipe to start");
                    }
                }
                StationStatus::WaitingToScald | StationStatus::WaitingToAttack => {
                    let recipe = unwrap!(ctx.recipe.as_ref(), "no recipe when waiting?!");
                    ctx.station_status.advance();
                    ctx.station_status_signal.signal(ctx.station_status);

                    let event = match ctx.station_status {
                        StationStatus::Scalding => RecipeExecutorEvent::ScheduleScald(
                            ctx.station,
                            unwrap!(recipe.scald, "no scald step?!").clone(),
                        ),
                        StationStatus::Attacking => RecipeExecutorEvent::ScheduleAttacks(
                            ctx.station,
                            recipe.attacks.clone(),
                        ),
                        _ => unreachable!("advancing from WaitingToX must result in status X"),
                    };

                    ctx.recipe_executor_channel.send(event).await;
                }
                StationStatus::Finished => {
                    reset_station(&mut ctx);
                    debug!("recipe ended");
                }
                _ => warn!("invalid button press"),
            },
            AdvanceRecipe => {
                ctx.station_status.advance();
                ctx.station_status_signal.signal(ctx.station_status);

                if let StationStatus::Finalizing = ctx.station_status {
                    let recipe = unwrap!(ctx.recipe.as_ref(), "no recipe when finalizing?!");
                    if let Some(duration) = recipe.finalization_time {
                        Timer::after(duration).await;
                    }
                    ctx.station_status = StationStatus::Finished;
                    ctx.station_status_signal.signal(ctx.station_status);
                }
            }
            CancelRecipe => match ctx.station_status {
                StationStatus::Free => {
                    warn!("no recipe to cancel");
                }
                _ => {
                    reset_station(&mut ctx);
                    ctx.recipe_executor_channel
                        .send(RecipeExecutorEvent::CancelRecipe(ctx.station))
                        .await;
                    debug!("recipe canceled");
                }
            },
            UpdateFixedRecipe(new_fixed_recipe) => ctx.fixed_recipe = new_fixed_recipe,
            UpdateRecipe(new_recipe) => {
                if ctx.recipe.is_none() {
                    ctx.recipe = Some(new_recipe)
                } else {
                    warn!("this station already has a recipe");
                }
            }
            _ => todo!(),
        }
    }
}

struct RecipeExecutorContext {
    step_queue: StepQueue,
    num_steps_per_station: PerStationData<usize>,
    recipe_executor_channel: &'static RecipeExecutorChannel,
    recipe_controller_channels: &'static PerStationStaticData<RecipeControllerChannel>,
}

struct RecipeControllerContext {
    station: Station,
    station_status: StationStatus,
    fixed_recipe: Option<Recipe>,
    recipe: Option<Recipe>,
    recipe_executor_channel: &'static RecipeExecutorChannel,
    recipe_controller_channel: &'static RecipeControllerChannel,
    station_status_signal: &'static StationStatusSignal,
}

pub fn spawn_tasks(spawner: &Spawner, ctx: &'static crate::GlobalContext) {
    let recipe_executor_channel = make_static!(RecipeExecutorChannel::new());
    unwrap!(spawner.spawn(recipe_executor_task(RecipeExecutorContext {
        num_steps_per_station: make_per_station_data!(0usize),
        step_queue: Vec::new(),
        recipe_executor_channel,
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

    for i in 0..MAX_NUM_STATIONS {
        unwrap!(
            spawner.spawn(recipe_controller_task(RecipeControllerContext {
                station_status: StationStatus::Free,
                fixed_recipe: None,
                recipe: default_recipe.clone(),
                station: Station(i),
                recipe_executor_channel,
                recipe_controller_channel: ctx.recipe_controller_channels[i],
                station_status_signal: ctx.station_status_signals[i],
            }))
        );
    }
}
