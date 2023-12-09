use defmt::*;
use embassy_executor::task;
use embassy_futures::select::{select, Either::*};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel};
use embassy_time::*;
use heapless::*;

use super::controller;
use super::*;

pub(super) struct Context {
    pub step_queue: StepQueue,
    pub num_steps_per_station: stations::PerStationData<usize>,
    pub recipe_executor_channel: &'static Channel,
    pub recipe_controller_channels: &'static stations::PerStationStaticData<controller::Channel>,
}

#[derive(Format)]
pub enum Event {
    ScheduleAttacks(stations::Station, RecipeSteps),
    ScheduleScald(stations::Station, Step),
    CancelRecipe(stations::Station),
}

#[derive(Format, Clone, Copy)]
pub struct StepExecutionInfo {
    pub instant: Instant,
    pub step: Step,
    pub station: stations::Station,
}

pub type Channel =
    channel::Channel<NoopRawMutex, Event, { MAX_EVENTS_PER_STATION * stations::MAX_STATIONS }>;

pub type StepQueue = Vec<StepExecutionInfo, { MAX_RECIPE_STEPS * stations::MAX_STATIONS }>;

const TRAVEL_TIME: Duration = Duration::from_secs(1);

fn cancel_recipe(ctx: &mut Context, station: stations::Station) {
    ctx.step_queue.retain(|info| info.station != station);
    ctx.num_steps_per_station[station.0] = 0;
    debug!("#{}: recipe was canceled", station);
}

fn step_has_collisions(step: &StepExecutionInfo, existing_steps: &[StepExecutionInfo]) -> bool {
    let new_end = step.instant + step.step.duration;
    for other_info in existing_steps.iter().rev() {
        let end = other_info.instant + other_info.step.duration;
        if other_info.instant > new_end {
            break;
        }

        let begin_collides = step.instant >= other_info.instant && step.instant <= end;
        let end_collides = new_end >= other_info.instant && new_end <= end;

        if begin_collides || end_collides {
            return true;
        }
    }
    return false;
}

fn interpret_event(ctx: &mut Context, event: Event) {
    use Event::*;
    match event {
        ScheduleScald(station, new_step) => {
            ctx.num_steps_per_station[station.0] += 1;

            if ctx.step_queue.is_empty() {
                unwrap!(ctx.step_queue.push(StepExecutionInfo {
                    instant: Instant::now() + TRAVEL_TIME,
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
                        continue;
                    }

                    let new_step_begin = end + TRAVEL_TIME;
                    let new_step_end = new_step_begin + new_step.duration;

                    // SAFETY: `i` is never 0 because of the `if at_last_step` check above
                    let next_step_begin = ctx.step_queue[i - 1].instant;
                    if new_step_end > next_step_begin - TRAVEL_TIME {
                        // no luck, move on to the next step
                        continue;
                    }

                    unwrap!(ctx.step_queue.insert(
                        i,
                        StepExecutionInfo {
                            instant: new_step_begin,
                            step: new_step,
                            station
                        }
                    ));
                    break;
                }
            }
        }
        ScheduleAttacks(station, steps) => {
            if steps.len() == 0 {
                error!("#{}: no steps to schedule", station);
                return;
            }

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

                        begin + step.duration + step.interval.unwrap_or_default()
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

                            begin + step.duration + step.interval.unwrap_or_default()
                        });
                        break;
                    }

                    if !info.step.interval.is_some() {
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
                        let mut mapped_steps = steps.iter().map(|step| {
                            let instant = accumulator;
                            accumulator += step.duration + step.interval.unwrap_or_default();
                            StepExecutionInfo {
                                instant,
                                step: *step,
                                station,
                            }
                        });

                        if mapped_steps
                            .any(|step_info| step_has_collisions(&step_info, &ctx.step_queue[0..i]))
                        {
                            continue;
                        }

                        // at this point the steps are guaranteed to not collide with any exisiting step
                        // so we can simply insert them anywhere the queue and sort it in descending order
                        ctx.step_queue.extend(mapped_steps);
                        ctx.step_queue
                            .sort_unstable_by(|info_a, info_b| info_b.instant.cmp(&info_a.instant));

                        break 'outer;
                    }
                }
            }
        }
        CancelRecipe(station) => cancel_recipe(ctx, station),
    }

    trace!("event handled - step queue = {}", ctx.step_queue);
}

async fn execute_step(ctx: &mut Context, info: &StepExecutionInfo) {
    if let Some(delta) = Instant::now().checked_duration_since(info.instant) {
        warn!(
            "#{}: current step is in the past [delta = {}]",
            info.station, delta
        );

        // compensate by moving all the remaining steps in the queue forward
        for info in ctx.step_queue.iter_mut() {
            info.instant += Duration::from_ticks(delta.as_ticks());
        }
    }

    debug!("#{}: step begin", info.station);

    Timer::after(info.step.duration).await;

    debug!("#{}: step end", info.station);

    if ctx.num_steps_per_station[info.station.0] == 0 {
        ctx.recipe_controller_channels[info.station.0]
            .send(controller::Event::AdvanceRecipe)
            .await;
    }
}

#[task]
pub(super) async fn main_task(mut ctx: Context) {
    loop {
        if ctx.step_queue.is_empty() {
            let event = ctx.recipe_executor_channel.receive().await;
            interpret_event(&mut ctx, event);
        } else {
            // SAFETY: we checked that the queue is not empty
            let info = unsafe { ctx.step_queue.pop_unchecked() };
            ctx.num_steps_per_station[info.station.0] -= 1;

            // keep interpreting events while waiting for the step to be executed
            loop {
                match select(
                    Timer::at(info.instant - TRAVEL_TIME),
                    ctx.recipe_executor_channel.receive(),
                )
                .await
                {
                    First(..) => {
                        // FIXME: go to station
                        Timer::at(info.instant).await;
                        execute_step(&mut ctx, &info).await;
                        break;
                    }
                    Second(event) => {
                        if let Event::CancelRecipe(canceled_station) = event {
                            // recipe got canceled while we were waiting for the step to happen
                            // messed up stuff...
                            if canceled_station == info.station {
                                cancel_recipe(&mut ctx, info.station);
                                break;
                            }
                        }
                        interpret_event(&mut ctx, event);
                    }
                }
            }
        }
    }
}
