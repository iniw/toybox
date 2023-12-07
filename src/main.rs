#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use core::cmp::Ordering;

use defmt::*;
use defmt::{todo, unreachable};
use defmt_rtt as _;
use embassy_executor::{main, task, Spawner};
use embassy_futures::select::{select, Either::*};
use embassy_stm32::{exti, gpio::*};
use embassy_sync::channel::*;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use embassy_time::{Duration, Instant, Timer};
use heapless::Vec;
use panic_probe as _;
use static_cell::make_static;

#[task(pool_size = MAX_NUM_STATIONS)]
async fn button_task(
    pin: AnyPin,
    exti: exti::AnyChannel,
    recipe_queue_signal: &'static RecipeControllerSignal,
) {
    let input = Input::new(pin, Pull::Up);
    let mut button = exti::ExtiInput::new(input, exti);

    loop {
        button.wait_for_rising_edge().await;
        debug!("button pressed");

        match select(button.wait_for_falling_edge(), Timer::after_secs(3)).await {
            First(_) => {
                debug!("button released");
                recipe_queue_signal.signal(RecipeControllerEvent::ButtonPress);
            }
            Second(_) => {
                debug!("button was held");
                recipe_queue_signal.signal(RecipeControllerEvent::CancelRecipe);
            }
        }

        // disallow quick button presses since the signal is edge-triggered
        // so we might run into false-positives on bad hardware
        Timer::after_millis(100).await;
    }
}

#[task(pool_size = MAX_NUM_STATIONS)]
async fn led_task(p: AnyPin, led_signal: &'static LedSignal) {
    let mut led = Output::new(p, Level::Low, Speed::Low);
    let mut led_status = LedStatus::Off;

    loop {
        match led_status {
            LedStatus::Blinking => {
                match select(Timer::after_millis(500), led_signal.wait()).await {
                    First(_) => {
                        led.toggle();
                        debug!("blink");
                    }
                    Second(signal) => {
                        led_status = signal;
                    }
                }
            }
            LedStatus::On | LedStatus::Off => {
                led.set_level(led_status.into());
                debug!("led status = {}", led_status);
                led_status = led_signal.wait().await;
            }
        }
    }
}

#[task(pool_size = MAX_NUM_STATIONS)]
async fn station_status_task(
    station_status_signal: &'static StationStatusSignal,
    led_signal: &'static LedSignal,
) {
    loop {
        let new_status = station_status_signal.wait().await;
        debug!("new status = {}", new_status);
        match new_status {
            StationStatus::Free => {
                led_signal.signal(LedStatus::Off);
            }
            StationStatus::WaitingToAttack
            | StationStatus::WaitingToScald
            | StationStatus::Finished => {
                led_signal.signal(LedStatus::Blinking);
            }
            _ => {
                led_signal.signal(LedStatus::On);
            }
        }
    }
}

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
        RecipeExecutorEvent::ScheduleSteps(station, steps) => {
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
    if delta > 0 {
        warn!("current step is in the past - delta = {}", delta);

        for (instant, ..) in step_queue.iter_mut() {
            *instant += Duration::from_millis(delta);
        }
    }

    execute_step(&step, station, step_queue, recipe_controller_signal).await;
}

#[task]
async fn recipe_executor_task(
    recipe_executor_signal: &'static RecipeExecutorChannel,
    recipe_controller_signal: &'static [&'static RecipeControllerSignal; MAX_NUM_STATIONS],
) {
    let mut step_queue = StepQueue::new();
    loop {
        match step_queue.is_empty() {
            true => {
                // no steps to execute, just wait for an event
                let event = recipe_executor_signal.receive().await;
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
                            recipe_controller_signal[station.0],
                            &mut step_queue,
                            now_as_millis(),
                            instant_as_millis,
                        )
                        .await
                    }

                    Ordering::Greater => {
                        while let Second(event) =
                            select(Timer::at(instant), recipe_executor_signal.receive()).await
                        {
                            handle_executor_event(&mut step_queue, event);

                            // missed the timer while executing the event
                            if now_as_millis() >= instant_as_millis {
                                execute_missed_step(
                                    &step,
                                    station,
                                    recipe_controller_signal[station.0],
                                    &mut step_queue,
                                    now_as_millis(),
                                    instant_as_millis,
                                )
                                .await;

                                return;
                            }
                        }

                        execute_step(
                            &step,
                            station,
                            &step_queue,
                            recipe_controller_signal[station.0],
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
async fn recipe_controller_task(
    station: Station,
    recipe_controller_signal: &'static RecipeControllerSignal,
    recipe_executor_channel: &'static RecipeExecutorChannel,
    station_status_signal: &'static StationStatusSignal,
) {
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
        match recipe_controller_signal.wait().await {
            RecipeControllerEvent::ButtonPress => match station_status {
                // try initiating a new recipe
                StationStatus::Free => {
                    // if there is no host-sent recipe try using the fixed one
                    if recipe.is_none() {
                        recipe = fixed_recipe.clone();
                    }

                    if let Some(recipe) = &recipe {
                        if recipe.has_scald() {
                            station_status = StationStatus::WaitingToScald;
                            station_status_signal.signal(station_status);
                        } else {
                            station_status = StationStatus::Attacking;
                            station_status_signal.signal(station_status);

                            recipe_executor_channel
                                .send(RecipeExecutorEvent::ScheduleSteps(
                                    station,
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
                        station_status_signal.signal(station_status);

                        let event = match station_status {
                            StationStatus::Scalding => RecipeExecutorEvent::ScheduleScald(
                                station,
                                unwrap!(recipe.scald).clone(),
                            ),
                            StationStatus::Attacking => {
                                RecipeExecutorEvent::ScheduleSteps(station, recipe.attacks.clone())
                            }
                            _ => unreachable!(),
                        };

                        recipe_executor_channel.send(event).await;
                    } else {
                        error!("no recipe to continue?!");
                    }
                }
                StationStatus::Finished => {
                    reset_station(&mut recipe, &mut station_status, station_status_signal);
                    debug!("recipe ended");
                }
                _ => warn!("invalid button press"),
            },
            RecipeControllerEvent::AdvanceRecipe => {
                station_status.advance();
                station_status_signal.signal(station_status);

                match station_status {
                    StationStatus::Finalizing => {
                        if let Some(recipe) = &recipe {
                            if let Some(finalization_time) = recipe.finalization_time {
                                // FIXME: use a proper event instead of creating a fake scald
                                recipe_executor_channel
                                    .send(RecipeExecutorEvent::ScheduleScald(
                                        station,
                                        Step {
                                            duration: finalization_time,
                                            interval: None,
                                        },
                                    ))
                                    .await;
                            } else {
                                station_status = StationStatus::Finished;
                                station_status_signal.signal(station_status);
                            }
                        } else {
                            error!("no recipe to finalize?!");
                        }
                    }
                    StationStatus::Finished => {
                        reset_station(&mut recipe, &mut station_status, station_status_signal);
                        debug!("recipe ended");
                    }
                    _ => debug!("recipe advanced"),
                }
            }
            RecipeControllerEvent::CancelRecipe => match station_status {
                StationStatus::Free => {
                    warn!("no recipe to cancel");
                }
                _ => {
                    reset_station(&mut recipe, &mut station_status, station_status_signal);
                    debug!("recipe canceled");
                }
            },
            RecipeControllerEvent::UpdateFixedRecipe(new_fixed_recipe) => {
                fixed_recipe = new_fixed_recipe
            }
            RecipeControllerEvent::UpdateRecipe(new_recipe) => {
                if recipe.is_none() {
                    recipe = Some(new_recipe)
                } else {
                    warn!("this station already has a recipe");
                }
            }
        }
    }
}

const MAX_NUM_STATIONS: usize = 1;
type StationData<T> = [T; MAX_NUM_STATIONS];

#[derive(Format, Clone, Copy)]
enum LedStatus {
    On,
    Off,
    Blinking,
}

impl Into<Level> for LedStatus {
    fn into(self) -> Level {
        match self {
            LedStatus::On => Level::High,
            LedStatus::Off => Level::Low,
            LedStatus::Blinking => unreachable!("not in a binary state"),
        }
    }
}

type LedSignal = Signal<NoopRawMutex, LedStatus>;

#[derive(Format, Clone, Copy)]
struct Step {
    duration: Duration,
    interval: Option<Duration>,
}

#[derive(Format, Clone)]
struct Recipe {
    scald: Option<Step>,
    attacks: RecipeSteps,
    finalization_time: Option<Duration>,
}

impl Recipe {
    const MAX_STEPS: usize = 10;

    fn has_scald(&self) -> bool {
        return self.scald.is_some();
    }
}

type RecipeSteps = Vec<Step, { Recipe::MAX_STEPS }>;

#[derive(Format)]
enum RecipeControllerEvent {
    AdvanceRecipe,
    ButtonPress,
    CancelRecipe,
    UpdateRecipe(Recipe),
    UpdateFixedRecipe(Option<Recipe>),
}

type RecipeControllerSignal = Signal<NoopRawMutex, RecipeControllerEvent>;

#[derive(Format, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
struct Station(usize);

#[derive(Format)]
enum RecipeExecutorEvent {
    ScheduleSteps(Station, RecipeSteps),
    ScheduleScald(Station, Step),
    AdvanceRecipe(Station),
    CancelRecipe(Station),
}

type RecipeExecutorChannel = Channel<NoopRawMutex, RecipeExecutorEvent, MAX_NUM_STATIONS>;

#[derive(Format, Clone, Copy)]
enum StationStatus {
    Free,
    WaitingToScald,
    Scalding,
    WaitingToAttack,
    Attacking,
    Finalizing,
    Finished,
}

impl StationStatus {
    fn advance(&mut self) {
        *self = match self {
            StationStatus::WaitingToScald => StationStatus::Scalding,
            StationStatus::Scalding => StationStatus::WaitingToAttack,
            StationStatus::WaitingToAttack => StationStatus::Attacking,
            StationStatus::Attacking => StationStatus::Finalizing,
            StationStatus::Finalizing => StationStatus::Finished,
            _ => unreachable!("cannot advance finished recipe"),
        }
    }
}

type StationStatusSignal = Signal<NoopRawMutex, StationStatus>;

#[main]
async fn main(spawner: Spawner) {
    info!("hi");

    let p = embassy_stm32::init(Default::default());

    let array: StationData<&'static RecipeControllerSignal> =
        [make_static!(RecipeControllerSignal::new()); MAX_NUM_STATIONS];
    let recipe_controller_signals: &'static StationData<&'static RecipeControllerSignal> =
        make_static!(array);

    let recipe_executor_signals: StationData<&'static RecipeExecutorChannel> =
        [make_static!(Channel::new()); MAX_NUM_STATIONS];

    let station_status_signals: StationData<&'static StationStatusSignal> =
        [make_static!(Signal::new()); MAX_NUM_STATIONS];

    for i in 0..MAX_NUM_STATIONS {
        unwrap!(spawner.spawn(recipe_controller_task(
            Station(i),
            recipe_controller_signals[i],
            recipe_executor_signals[i],
            station_status_signals[i]
        )));

        unwrap!(spawner.spawn(recipe_executor_task(
            recipe_executor_signals[i],
            recipe_controller_signals
        )));
    }

    let led_signals: StationData<&'static LedSignal> =
        [make_static!(Signal::new()); MAX_NUM_STATIONS];
    let led_pins: StationData<AnyPin> = [p.PC6.degrade()];
    for (pin, led_signal) in led_pins.into_iter().zip(led_signals.iter()) {
        unwrap!(spawner.spawn(led_task(pin, led_signal)));
    }

    for (station_status_signal, led_signal) in station_status_signals.iter().zip(led_signals) {
        unwrap!(spawner.spawn(station_status_task(station_status_signal, led_signal)))
    }

    let button_pins: StationData<(AnyPin, exti::AnyChannel)> =
        [(p.PA0.degrade(), exti::Channel::degrade(p.EXTI0))];
    for ((pin, channel), recipe_queue_signal) in button_pins
        .into_iter()
        .zip(recipe_controller_signals.iter())
    {
        unwrap!(spawner.spawn(button_task(pin, channel, recipe_queue_signal)));
    }
}
