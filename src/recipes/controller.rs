use defmt::{unreachable, *};
use embassy_executor::task;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel};
use embassy_time::*;

use super::executor;
use super::*;

pub(super) struct Context {
    pub station: stations::Station,
    pub station_status: stations::Status,
    pub fixed_recipe: Option<Recipe>,
    pub recipe: Option<Recipe>,
    pub recipe_controller_channel: &'static Channel,
    pub recipe_executor_channel: &'static executor::Channel,
    pub station_status_signal: &'static stations::Signal,
}

#[derive(Format)]
pub enum Event {
    AdvanceRecipe,
    ButtonPress,
    CancelRecipe,
    #[allow(dead_code)]
    UpdateRecipe(Recipe),
    #[allow(dead_code)]
    UpdateFixedRecipe(Option<Recipe>),
}

pub type Channel = channel::Channel<NoopRawMutex, Event, MAX_EVENTS_PER_STATION>;

fn reset_station(ctx: &mut Context) {
    ctx.recipe = None;
    ctx.station_status = stations::Status::Free;
    ctx.station_status_signal.signal(ctx.station_status);
}

#[task(pool_size = stations::MAX_STATIONS)]
pub(super) async fn main_task(mut ctx: Context) {
    use Event::*;
    loop {
        match ctx.recipe_controller_channel.receive().await {
            ButtonPress => match ctx.station_status {
                // try initiating a new recipe
                stations::Status::Free => {
                    // if there is no host-sent recipe try using the fixed one
                    if ctx.recipe.is_none() {
                        ctx.recipe = ctx.fixed_recipe.clone();
                    }

                    if let Some(recipe) = &ctx.recipe {
                        if recipe.has_scald() {
                            ctx.station_status = stations::Status::WaitingToScald;
                            ctx.station_status_signal.signal(ctx.station_status);
                        } else {
                            ctx.station_status = stations::Status::Attacking;
                            ctx.station_status_signal.signal(ctx.station_status);

                            ctx.recipe_executor_channel
                                .send(executor::Event::ScheduleAttacks(
                                    ctx.station,
                                    recipe.attacks.clone(),
                                ))
                                .await;
                        }
                    } else {
                        warn!("#{}: no recipe to start", ctx.station);
                    }
                }
                stations::Status::WaitingToScald | stations::Status::WaitingToAttack => {
                    if ctx.recipe.is_none() {
                        error!("#{}: no recipe to advance?!", ctx.station);
                        continue;
                    }

                    // SAFETY: we checked that there is a recipe
                    let recipe = unsafe { ctx.recipe.as_ref().unwrap_unchecked() };
                    ctx.station_status.advance();
                    ctx.station_status_signal.signal(ctx.station_status);

                    let event = match ctx.station_status {
                        stations::Status::Scalding => {
                            if recipe.scald.is_none() {
                                error!("#{}: no scald step to schedule?!", ctx.station);
                                continue;
                            }
                            executor::Event::ScheduleScald(
                                ctx.station,
                                unsafe { recipe.scald.unwrap_unchecked() }.clone(),
                            )
                        }
                        stations::Status::Attacking => {
                            executor::Event::ScheduleAttacks(ctx.station, recipe.attacks.clone())
                        }
                        _ => unreachable!(
                            "advancing from WaitingToX must result in stations::Status X"
                        ),
                    };

                    ctx.recipe_executor_channel.send(event).await;
                }
                stations::Status::Finished => {
                    reset_station(&mut ctx);
                }
                _ => warn!("#{}: invalid button press", ctx.station),
            },
            AdvanceRecipe => {
                ctx.station_status.advance();
                ctx.station_status_signal.signal(ctx.station_status);

                if let stations::Status::Finalizing = ctx.station_status {
                    if ctx.recipe.is_none() {
                        error!("#{}: no recipe to finalize?!", ctx.station);
                        continue;
                    }

                    // SAFETY: we checked that there is a recipe
                    let recipe = unsafe { ctx.recipe.as_ref().unwrap_unchecked() };
                    if let Some(duration) = recipe.finalization_time {
                        // FIXME: handle cancelation mid-waiting
                        Timer::after(duration).await;
                        debug!("#{}: recipe ended", ctx.station);
                    }

                    ctx.station_status = stations::Status::Finished;
                    ctx.station_status_signal.signal(ctx.station_status);
                }
            }
            CancelRecipe => match ctx.station_status {
                stations::Status::Free => {
                    warn!("#{}: no recipe to cancel", ctx.station);
                }
                _ => {
                    reset_station(&mut ctx);
                    ctx.recipe_executor_channel
                        .send(executor::Event::CancelRecipe(ctx.station))
                        .await;
                }
            },
            UpdateFixedRecipe(new_fixed_recipe) => ctx.fixed_recipe = new_fixed_recipe,
            UpdateRecipe(new_recipe) => {
                if ctx.recipe.is_none() {
                    ctx.recipe = Some(new_recipe)
                } else {
                    warn!("#{}: station already has a recipe", ctx.station);
                }
            }
        }
    }
}
