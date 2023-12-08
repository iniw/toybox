use core::cmp::Ordering;

use defmt::{debug, error, unwrap, warn, Format};
use embassy_executor::{task, Spawner};
use embassy_futures::select::{select, Either::*};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel, signal::Signal};
use embassy_time::{Duration, Instant, Timer};
use heapless::{String, Vec};
use static_cell::make_static;

use crate::stations::*;
#[derive(Format, Clone, Copy)]
pub struct Step {
    duration: Duration,
    interval: Option<Duration>,
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

pub type RecipeControllerSignal = Signal<NoopRawMutex, RecipeControllerEvent>;

#[derive(Format)]
pub enum RecipeExecutorEvent {
    ScheduleAttacks(Station, RecipeSteps),
    ScheduleScald(Station, Step),
    #[allow(dead_code)]
    AdvanceRecipe(Station),
    #[allow(dead_code)]
    CancelRecipe(Station),
}

pub type RecipeExecutorChannel = Channel<NoopRawMutex, RecipeExecutorEvent, MAX_NUM_STATIONS>;

const TRAVEL_TIME: Duration = Duration::from_secs(1);
type StepQueue = Vec<(Instant, Step, Station), { Recipe::MAX_STEPS * MAX_NUM_STATIONS }>;

fn handle_executor_event(step_queue: &mut StepQueue, event: RecipeExecutorEvent) {
    match event {
        RecipeExecutorEvent::ScheduleScald(station, new_step) => {
            if step_queue.is_empty() {
                unwrap!(step_queue.push((Instant::now(), new_step, station)));
            } else {
                for (i, (begin, step, _)) in step_queue.iter().enumerate().rev() {
                    let end = *begin + step.duration;

                    // because `rev` is called after `enumerate` the last iteration is at index 0
                    let at_last_step = i == 0;
                    if at_last_step {
                        let new_step_begin = end + TRAVEL_TIME;
                        unwrap!(step_queue.insert(0, (new_step_begin, new_step, station)));
                        break;
                    }

                    if !step.interval.is_some() {
                        // no way to execute the step if there is no interval between the current + next
                        continue;
                    }

                    let new_step_begin = end + TRAVEL_TIME;
                    let new_step_end = new_step_begin + new_step.duration;

                    // SAFETY: `i` is never 0 because of the `if at_last_step` check above
                    let next_step_begin = step_queue[i - 1].0;
                    if new_step_end < next_step_begin - TRAVEL_TIME {
                        unwrap!(step_queue.insert(i, (new_step_begin, new_step, station)));
                        break;
                    } else {
                        // no luck, move on to the next step
                        continue;
                    }
                }
            }
        }
        RecipeExecutorEvent::ScheduleAttacks(station, steps) => {
            if step_queue.is_empty() {
                let _ = steps
                    .iter()
                    .fold(Instant::now() + TRAVEL_TIME, |begin, step| {
                        unwrap!(step_queue.insert(0, (begin, *step, station)));
                        begin + step.duration + step.interval.unwrap_or(Duration::MIN /* 0 */)
                    });
            } else {
                'outer: for (i, (begin, step, _)) in step_queue.iter().enumerate().rev() {
                    let end = *begin + step.duration;

                    // because `rev` is called after `enumerate` the last iteration is at index 0
                    let at_last_step = i == 0;
                    if at_last_step {
                        let _ = steps.iter().fold(end + TRAVEL_TIME, |begin, step| {
                            unwrap!(step_queue.insert(0, (begin, *step, station)));
                            begin
                                + step.duration
                                + step.interval.unwrap_or(Duration::MIN /* 0 */)
                        });
                        break;
                    }

                    if !step.interval.is_some() {
                        // cant fit in the steps if there is no interval between the current and next step
                        continue;
                    }

                    let next_step_begin = step_queue[i - 1].0;

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
                            accumulator +=
                                step.duration + step.interval.unwrap_or(Duration::MIN /* 0 */);
                            (instant, *step, station)
                        });

                        if queue_steps.any(|(new_begin, new_step, _)| {
                            let new_end = new_begin + new_step.duration;
                            for (begin, step, _) in step_queue[0..i].iter().rev() {
                                let end = *begin + step.duration;
                                if *begin > new_end {
                                    break;
                                }

                                let begin_collides = new_begin >= *begin && new_begin <= end;
                                let end_collides = new_end >= *begin && new_end <= end;

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
                        step_queue.extend(queue_steps);
                        step_queue[..].sort_unstable_by(|(a, ..), (b, ..)| b.cmp(a));

                        break 'outer;
                    }
                }
            }
        }
        _ => todo!(),
    }

    debug!("event handled - step queue = {:?}", step_queue);
}

async fn execute_step(
    step: &Step,
    station: Station,
    step_queue: &StepQueue,
    recipe_controller_signal: &RecipeControllerSignal,
) {
    debug!("step begin");
    Timer::after(step.duration).await;
    debug!("step end");

    // FIXME: this is silly and stupid and inneficient
    if !step_queue
        .iter()
        .any(|(.., other_station)| *other_station == station)
    {
        recipe_controller_signal.signal(RecipeControllerEvent::AdvanceRecipe);
    }
}

async fn execute_missed_step(
    step: &Step,
    station: Station,
    recipe_controller_signal: &RecipeControllerSignal,
    step_queue: &mut StepQueue,
    now_as_millis: u64,
    instant_as_millis: u64,
) {
    let delta = now_as_millis - instant_as_millis;
    // this function may get called with `now_as_millis` == `instant_as_millis`, which just makes the loop a waste of time
    if delta > 0 {
        warn!("current step is in the past - delta = {}", delta);

        // compensate by moving all the remaining steps in the queue forward
        for (instant, ..) in step_queue.iter_mut() {
            *instant += Duration::from_millis(delta);
        }
    }

    execute_step(&step, station, step_queue, recipe_controller_signal).await;
}

#[task]
async fn recipe_executor_task(ctx: RecipeExecutorContext) {
    let mut step_queue = StepQueue::new();
    loop {
        match step_queue.is_empty() {
            true => {
                // no steps to execute, just wait for an event
                let event = ctx.recipe_executor_channel.receive().await;
                handle_executor_event(&mut step_queue, event);
            }
            false => {
                let (instant, step, station) = unwrap!(step_queue.pop());

                // `Instant` has way more precision than we care about.
                // to avoid tripping up the `Ordering::Less` case below it gets converted to milliseconds.
                // in that process it gets rounded down and so it needs to be incremented by 1,
                // otherwise its consistently 1ms in the past
                let instant_as_millis = instant.as_millis() + 1;
                let now_as_millis = || Instant::now().as_millis();

                match instant_as_millis.cmp(&now_as_millis()) {
                    Ordering::Less | Ordering::Equal => {
                        execute_missed_step(
                            &step,
                            station,
                            ctx.recipe_controller_signals[station.0],
                            &mut step_queue,
                            now_as_millis(),
                            instant_as_millis,
                        )
                        .await
                    }
                    Ordering::Greater => {
                        // the instant hasn't arrived yet, but we can't just wait for it since we have to keep receiving events
                        // this loop makes us do both at the same time
                        while let Second(event) =
                            select(Timer::at(instant), ctx.recipe_executor_channel.receive()).await
                        {
                            handle_executor_event(&mut step_queue, event);

                            // interpreting the event took long enough for the step instant to be missed
                            if now_as_millis() >= instant_as_millis {
                                execute_missed_step(
                                    &step,
                                    station,
                                    ctx.recipe_controller_signals[station.0],
                                    &mut step_queue,
                                    now_as_millis(),
                                    instant_as_millis,
                                )
                                .await;

                                return;
                            }
                        }

                        // falling through the `while` above means that the instant has arrived
                        execute_step(
                            &step,
                            station,
                            &step_queue,
                            ctx.recipe_controller_signals[station.0],
                        )
                        .await;
                    }
                }
            }
        }
    }
}

fn reset_station(
    recipe: &mut Option<Recipe>,
    station_status: &mut StationStatus,
    station_status_signal: &'static StationStatusSignal,
) {
    *recipe = None;
    *station_status = StationStatus::Free;
    station_status_signal.signal(*station_status);
}

#[task(pool_size = MAX_NUM_STATIONS)]
async fn recipe_controller_task(ctx: RecipeControllerContext) {
    let mut station_status = StationStatus::Free;
    let mut fixed_recipe: Option<Recipe> = None;

    // default recipe for testing, will be set to None when finished
    let mut recipe: Option<Recipe> = Some(Recipe {
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

    loop {
        use RecipeControllerEvent::*;
        match ctx.recipe_controller_signal.wait().await {
            ButtonPress => match station_status {
                // try initiating a new recipe
                StationStatus::Free => {
                    // if there is no host-sent recipe try using the fixed one
                    if recipe.is_none() {
                        recipe = fixed_recipe.clone();
                    }

                    if let Some(recipe) = &recipe {
                        if recipe.has_scald() {
                            station_status = StationStatus::WaitingToScald;
                            ctx.station_status_signal.signal(station_status);
                        } else {
                            station_status = StationStatus::Attacking;
                            ctx.station_status_signal.signal(station_status);

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
                    if let Some(recipe) = &recipe {
                        station_status.advance();
                        ctx.station_status_signal.signal(station_status);

                        let event = match station_status {
                            StationStatus::Scalding => RecipeExecutorEvent::ScheduleScald(
                                ctx.station,
                                unwrap!(recipe.scald).clone(),
                            ),
                            StationStatus::Attacking => RecipeExecutorEvent::ScheduleAttacks(
                                ctx.station,
                                recipe.attacks.clone(),
                            ),
                            _ => unreachable!(),
                        };

                        ctx.recipe_executor_channel.send(event).await;
                    } else {
                        error!("no recipe to continue?!");
                        reset_station(&mut recipe, &mut station_status, ctx.station_status_signal);
                    }
                }
                StationStatus::Finished => {
                    reset_station(&mut recipe, &mut station_status, ctx.station_status_signal);
                    debug!("recipe ended");
                }
                _ => warn!("invalid button press"),
            },
            AdvanceRecipe => {
                station_status.advance();
                ctx.station_status_signal.signal(station_status);

                match station_status {
                    StationStatus::Finalizing => {
                        if let Some(some_recipe) = &recipe {
                            match some_recipe.finalization_time {
                                Some(duration) => {
                                    Timer::after(duration).await;
                                    reset_station(
                                        &mut recipe,
                                        &mut station_status,
                                        ctx.station_status_signal,
                                    );
                                    debug!("recipe #{} - finalization time elapsed", ctx.station);
                                }
                                None => {
                                    station_status = StationStatus::Finished;
                                    ctx.station_status_signal.signal(station_status);
                                }
                            }
                        } else {
                            error!("no recipe to finalize?!");
                            reset_station(
                                &mut recipe,
                                &mut station_status,
                                ctx.station_status_signal,
                            );
                        }
                    }
                    StationStatus::Finished => {
                        reset_station(&mut recipe, &mut station_status, ctx.station_status_signal);
                        debug!("recipe ended");
                    }
                    _ => debug!("recipe advanced"),
                }
            }
            CancelRecipe => match station_status {
                StationStatus::Free => {
                    warn!("no recipe to cancel");
                }
                _ => {
                    reset_station(&mut recipe, &mut station_status, ctx.station_status_signal);
                    debug!("recipe canceled");
                }
            },
            UpdateFixedRecipe(new_fixed_recipe) => fixed_recipe = new_fixed_recipe,
            UpdateRecipe(new_recipe) => {
                if recipe.is_none() {
                    recipe = Some(new_recipe)
                } else {
                    warn!("this station already has a recipe");
                }
            }
            _ => todo!(),
        }
    }
}

struct RecipeExecutorContext {
    recipe_executor_channel: &'static RecipeExecutorChannel,
    recipe_controller_signals: &'static PerStationStaticData<RecipeControllerSignal>,
}

struct RecipeControllerContext {
    station: Station,
    recipe_executor_channel: &'static RecipeExecutorChannel,
    recipe_controller_signal: &'static RecipeControllerSignal,
    station_status_signal: &'static StationStatusSignal,
}

pub fn spawn_tasks(spawner: &Spawner, ctx: &'static crate::GlobalContext) {
    let recipe_executor_channel = make_static!(RecipeExecutorChannel::new());
    unwrap!(spawner.spawn(recipe_executor_task(RecipeExecutorContext {
        recipe_executor_channel,
        recipe_controller_signals: &ctx.recipe_controller_signals,
    })));

    for i in 0..MAX_NUM_STATIONS {
        unwrap!(
            spawner.spawn(recipe_controller_task(RecipeControllerContext {
                station: Station(i),
                recipe_executor_channel,
                recipe_controller_signal: ctx.recipe_controller_signals[i],
                station_status_signal: ctx.station_status_signals[i],
            }))
        );
    }
}
