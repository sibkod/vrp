use super::*;
use crate::construction::enablers::{JobTie, VehicleTie};
use crate::construction::features::*;
use hashbrown::HashSet;
use vrp_core::construction::clustering::vicinity::ClusterDimension;
use vrp_core::construction::enablers::NoRouteIntervals;
use vrp_core::construction::features::*;
use vrp_core::models::common::{LoadOps, MultiDimLoad, SingleDimLoad};
use vrp_core::models::problem::{Actor, Single, TransportCost};
use vrp_core::models::{Feature, Goal, GoalContext};

pub(super) fn create_goal_context(
    api_problem: &ApiProblem,
    blocks: &ProblemBlocks,
    props: &ProblemProperties,
) -> Result<GoalContext, GenericError> {
    let mut features = Vec::new();

    // TODO what's about performance implications on order of features when they are evaluated?

    let objective_features = get_objective_features(api_problem, blocks, props)?;
    let (global_objective_map, local_objective_map) = extract_feature_map(objective_features.as_slice())?;
    features.extend(objective_features.into_iter().flat_map(|features| features.into_iter()));

    if props.has_unreachable_locations {
        features.push(create_reachable_feature("reachable", blocks.transport.clone(), REACHABLE_CONSTRAINT_CODE)?)
    }

    features.push(get_capacity_feature("capacity", api_problem, blocks, props)?);

    if props.has_tour_travel_limits {
        features.push(get_tour_limit_feature("tour_limit", api_problem, blocks.transport.clone())?)
    }

    if props.has_breaks {
        features.push(create_optional_break_feature("break", BREAK_CONSTRAINT_CODE)?)
    }

    if props.has_recharges {
        features.push(get_recharge_feature("recharge", api_problem, blocks.transport.clone())?);
    }

    if props.has_order && !global_objective_map.iter().flat_map(|o| o.iter()).any(|name| *name == "tour_order") {
        features.push(create_tour_order_hard_feature("tour_order", TOUR_ORDER_CONSTRAINT_CODE, get_tour_order_fn())?)
    }

    if props.has_compatibility {
        features.push(create_compatibility_feature("compatibility", COMPATIBILITY_CONSTRAINT_CODE, COMPATIBILITY_KEY)?);
    }

    if props.has_group {
        features.push(create_group_feature("group", blocks.jobs.size(), GROUP_CONSTRAINT_CODE, GROUP_KEY)?);
    }

    if props.has_skills {
        features.push(create_skills_feature("skills", SKILL_CONSTRAINT_CODE)?)
    }

    if !blocks.locks.is_empty() {
        features.push(create_locked_jobs_feature(
            "locked_jobs",
            blocks.fleet.as_ref(),
            &blocks.locks,
            LOCKING_CONSTRAINT_CODE,
        )?);
    }

    if props.has_tour_size_limits {
        features.push(create_activity_limit_feature(
            "activity_limit",
            TOUR_SIZE_CONSTRAINT_CODE,
            Arc::new(|actor| actor.vehicle.dimens.get_tour_size()),
        )?);
    }

    let goal = Goal::no_alternatives(global_objective_map, local_objective_map);

    GoalContext::new(features.as_slice(), goal)
}

fn get_objective_features(
    api_problem: &ApiProblem,
    blocks: &ProblemBlocks,
    props: &ProblemProperties,
) -> Result<Vec<Vec<Feature>>, GenericError> {
    let objectives = get_objectives(api_problem, props);

    objectives
        .iter()
        .map(|objectives| {
            objectives
                .iter()
                .map(|objective| match objective {
                    Objective::MinimizeCost => create_minimize_transport_costs_feature(
                        "min_cost",
                        blocks.transport.clone(),
                        blocks.activity.clone(),
                        TIME_CONSTRAINT_CODE,
                    ),
                    Objective::MinimizeDistance => create_minimize_distance_feature(
                        "min_distance",
                        blocks.transport.clone(),
                        blocks.activity.clone(),
                        TIME_CONSTRAINT_CODE,
                    ),
                    Objective::MinimizeDuration => create_minimize_duration_feature(
                        "min_duration",
                        blocks.transport.clone(),
                        blocks.activity.clone(),
                        TIME_CONSTRAINT_CODE,
                    ),
                    Objective::MinimizeTours => create_minimize_tours_feature("min_tours"),
                    Objective::MaximizeTours => create_maximize_tours_feature("max_tours"),
                    Objective::MaximizeValue { breaks } => create_maximize_total_job_value_feature(
                        "max_value",
                        JobReadValueFn::Left(Arc::new({
                            let break_value = *breaks;
                            move |job| {
                                job.dimens().get_job_value().unwrap_or_else(|| {
                                    job.dimens()
                                        .get_job_type()
                                        .zip(break_value)
                                        .filter(|(job_type, _)| *job_type == "break")
                                        .map(|(_, break_value)| break_value)
                                        .unwrap_or(0.)
                                })
                            }
                        })),
                        Arc::new(|job, value| match job {
                            CoreJob::Single(single) => {
                                let mut dimens = single.dimens.clone();
                                dimens.set_job_value(Some(value));

                                CoreJob::Single(Arc::new(Single { places: single.places.clone(), dimens }))
                            }
                            _ => job.clone(),
                        }),
                        -1,
                    ),
                    Objective::MinimizeUnassigned { breaks } => create_minimize_unassigned_jobs_feature(
                        "min_unassigned",
                        Arc::new({
                            let break_value = *breaks;
                            let default_value = 1.;
                            move |_, job| {
                                if let Some(clusters) = job.dimens().get_cluster() {
                                    clusters.len() as f64 * default_value
                                } else {
                                    job.dimens().get_job_type().map_or(default_value, |job_type| {
                                        match job_type.as_str() {
                                            "break" => break_value.unwrap_or(default_value),
                                            "reload" => 0.,
                                            _ => default_value,
                                        }
                                    })
                                }
                            }
                        }),
                    ),
                    Objective::MinimizeArrivalTime => create_minimize_arrival_time_feature("min_arrival_time"),
                    Objective::BalanceMaxLoad { options } => {
                        if props.has_multi_dimen_capacity {
                            create_max_load_balanced_feature::<MultiDimLoad>(
                                "max_load_balance",
                                get_threshold(options),
                                Arc::new(|loaded, capacity| {
                                    let mut max_ratio = 0_f64;

                                    for (idx, value) in capacity.load.iter().enumerate() {
                                        let ratio = loaded.load[idx] as f64 / *value as f64;
                                        max_ratio = max_ratio.max(ratio);
                                    }

                                    max_ratio
                                }),
                            )
                        } else {
                            create_max_load_balanced_feature::<SingleDimLoad>(
                                "max_load_balance",
                                get_threshold(options),
                                Arc::new(|loaded, capacity| loaded.value as f64 / capacity.value as f64),
                            )
                        }
                    }
                    Objective::BalanceActivities { options } => {
                        create_activity_balanced_feature("activity_balance", get_threshold(options))
                    }
                    Objective::BalanceDistance { options } => {
                        create_distance_balanced_feature("distance_balance", get_threshold(options))
                    }
                    Objective::BalanceDuration { options } => {
                        create_duration_balanced_feature("duration_balance", get_threshold(options))
                    }
                    Objective::CompactTour { options } => {
                        let thresholds = Some((options.threshold, options.distance));
                        create_tour_compactness_feature(
                            "tour_compact",
                            blocks.jobs.clone(),
                            options.job_radius,
                            TOUR_COMPACTNESS_KEY,
                            thresholds,
                        )
                    }
                    Objective::TourOrder => {
                        create_tour_order_soft_feature("tour_order", TOUR_ORDER_KEY, get_tour_order_fn())
                    }
                    Objective::FastService { tolerance } => {
                        get_fast_service_feature("fast_service", blocks, props, *tolerance)
                    }
                })
                .collect()
        })
        .collect()
}

fn get_objectives(api_problem: &ApiProblem, props: &ProblemProperties) -> Vec<Vec<Objective>> {
    if let Some(objectives) = api_problem.objectives.clone() {
        objectives
    } else {
        let mut objectives = vec![
            vec![Objective::MinimizeUnassigned { breaks: Some(1.) }],
            vec![Objective::MinimizeTours],
            vec![Objective::MinimizeCost],
        ];

        if props.has_value {
            objectives.insert(0, vec![Objective::MaximizeValue { breaks: None }])
        }

        objectives
    }
}

const RELOAD_THRESHOLD: f64 = 0.9;

fn get_capacity_feature(
    name: &str,
    api_problem: &ApiProblem,
    blocks: &ProblemBlocks,
    props: &ProblemProperties,
) -> Result<Feature, GenericError> {
    if props.has_reloads {
        if props.has_multi_dimen_capacity {
            get_capacity_with_reload_feature::<MultiDimLoad>(
                name,
                api_problem,
                blocks,
                MultiDimLoad::new,
                Box::new(move |capacity| *capacity * RELOAD_THRESHOLD),
            )
        } else {
            get_capacity_with_reload_feature::<SingleDimLoad>(
                name,
                api_problem,
                blocks,
                |capacity| SingleDimLoad::new(capacity.first().cloned().unwrap_or_default()),
                Box::new(move |capacity| *capacity * RELOAD_THRESHOLD),
            )
        }
    } else if props.has_multi_dimen_capacity {
        create_capacity_limit_feature::<MultiDimLoad>(name, CAPACITY_CONSTRAINT_CODE)
    } else {
        create_capacity_limit_feature::<SingleDimLoad>(name, CAPACITY_CONSTRAINT_CODE)
    }
}

fn get_fast_service_feature(
    name: &str,
    blocks: &ProblemBlocks,
    props: &ProblemProperties,
    tolerance: Option<f64>,
) -> Result<Feature, GenericError> {
    let (transport, activity) = (blocks.transport.clone(), blocks.activity.clone());
    if props.has_reloads {
        if props.has_multi_dimen_capacity {
            create_fast_service_feature::<MultiDimLoad>(
                name,
                transport,
                activity,
                create_simple_reload_route_intervals(Box::new(move |capacity: &MultiDimLoad| {
                    *capacity * RELOAD_THRESHOLD
                })),
                tolerance,
                FAST_SERVICE_KEY,
            )
        } else {
            create_fast_service_feature::<SingleDimLoad>(
                name,
                transport,
                activity,
                create_simple_reload_route_intervals(Box::new(move |capacity: &SingleDimLoad| {
                    *capacity * RELOAD_THRESHOLD
                })),
                tolerance,
                FAST_SERVICE_KEY,
            )
        }
    } else {
        let route_intervals = Arc::new(NoRouteIntervals::default());
        if props.has_multi_dimen_capacity {
            create_fast_service_feature::<MultiDimLoad>(
                name,
                transport,
                activity,
                route_intervals,
                tolerance,
                FAST_SERVICE_KEY,
            )
        } else {
            create_fast_service_feature::<SingleDimLoad>(
                name,
                transport,
                activity,
                route_intervals,
                tolerance,
                FAST_SERVICE_KEY,
            )
        }
    }
}

fn get_capacity_with_reload_feature<T: LoadOps + SharedResource>(
    name: &str,
    api_problem: &ApiProblem,
    blocks: &ProblemBlocks,
    capacity_map: fn(Vec<i32>) -> T,
    load_schedule_threshold_fn: LoadScheduleThresholdFn<T>,
) -> Result<Feature, GenericError> {
    let reload_resources = get_reload_resources(api_problem, blocks.job_index.as_ref(), capacity_map);
    let capacity_feature_factory: CapacityFeatureFactoryFn = Box::new(|name, route_intervals| {
        create_capacity_limit_with_multi_trip_feature::<T>(name, CAPACITY_CONSTRAINT_CODE, route_intervals)
    });

    if reload_resources.is_empty() {
        create_simple_reload_multi_trip_feature(name, capacity_feature_factory, load_schedule_threshold_fn)
    } else {
        create_shared_reload_multi_trip_feature(
            name,
            capacity_feature_factory,
            load_schedule_threshold_fn,
            reload_resources,
            blocks.jobs.size(),
            RELOAD_RESOURCE_CONSTRAINT_CODE,
            RELOAD_RESOURCE_KEY,
        )
    }
}

fn get_tour_limit_feature(
    name: &str,
    api_problem: &ApiProblem,
    transport: Arc<dyn TransportCost + Send + Sync>,
) -> Result<Feature, GenericError> {
    let (distances, durations) = api_problem
        .fleet
        .vehicles
        .iter()
        .filter_map(|vehicle| vehicle.limits.as_ref().map(|limits| (vehicle, limits)))
        .fold((HashMap::new(), HashMap::new()), |(mut distances, mut durations), (vehicle, limits)| {
            limits.max_distance.iter().for_each(|max_distance| {
                distances.insert(vehicle.type_id.clone(), *max_distance);
            });

            limits.max_duration.iter().for_each(|max_duration| {
                durations.insert(vehicle.type_id.clone(), *max_duration);
            });

            (distances, durations)
        });

    let get_limit = |limit_map: HashMap<String, f64>| {
        Arc::new(move |actor: &Actor| {
            actor.vehicle.dimens.get_vehicle_type().and_then(|v_type| limit_map.get(v_type)).cloned()
        })
    };

    create_travel_limit_feature(
        name,
        transport.clone(),
        get_limit(distances),
        get_limit(durations),
        DISTANCE_LIMIT_CONSTRAINT_CODE,
        DURATION_LIMIT_CONSTRAINT_CODE,
    )
}

fn get_recharge_feature(
    name: &str,
    api_problem: &ApiProblem,
    transport: Arc<dyn TransportCost + Send + Sync>,
) -> Result<Feature, GenericError> {
    let distance_limit_index: HashMap<_, HashMap<_, _>> =
        api_problem.fleet.vehicles.iter().fold(HashMap::default(), |mut acc, vehicle_type| {
            vehicle_type
                .shifts
                .iter()
                .enumerate()
                .flat_map(|(shift_idx, shift)| {
                    shift.recharges.as_ref().map(|recharges| (shift_idx, recharges.max_distance))
                })
                .for_each(|(shift_idx, max_distance)| {
                    acc.entry(vehicle_type.type_id.clone()).or_default().insert(shift_idx, max_distance);
                });

            acc
        });

    let distance_limit_fn: RechargeDistanceLimitFn = Arc::new(move |actor: &Actor| {
        actor.vehicle.dimens.get_vehicle_type().zip(actor.vehicle.dimens.get_shift_index()).and_then(
            |(type_id, shift_idx)| distance_limit_index.get(type_id).and_then(|idx| idx.get(&shift_idx).copied()),
        )
    });

    create_recharge_feature(name, RECHARGE_CONSTRAINT_CODE, distance_limit_fn, transport)
}

fn get_reload_resources<T>(
    api_problem: &ApiProblem,
    job_index: &JobIndex,
    capacity_map: fn(Vec<i32>) -> T,
) -> HashMap<CoreJob, (T, SharedResourceId)>
where
    T: LoadOps + SharedResource,
{
    // get available resources
    let available_resources = api_problem
        .fleet
        .resources
        .as_ref()
        .iter()
        .flat_map(|resources| resources.iter())
        .map(|resource| match resource {
            VehicleResource::Reload { id, capacity } => (id.clone(), capacity.clone()),
        })
        .collect::<Vec<_>>();
    let total_resources_specified = available_resources.len();
    let available_resources = available_resources
        .into_iter()
        .enumerate()
        .map(|(idx, (id, capacity))| (id, (idx, capacity)))
        .collect::<HashMap<_, _>>();
    assert_eq!(total_resources_specified, available_resources.len());

    // get reload resources
    api_problem
        .fleet
        .vehicles
        .iter()
        .flat_map(|vehicle| {
            vehicle
                .shifts
                .iter()
                .enumerate()
                .flat_map(|(shift_idx, vehicle_shift)| {
                    vehicle_shift
                        .reloads
                        .iter()
                        .flatten()
                        .enumerate()
                        .map(move |(reload_idx, reload)| (shift_idx, reload_idx + 1, reload))
                })
                .filter_map(|(shift_idx, place_idx, reload)| {
                    reload
                        .resource_id
                        .as_ref()
                        .and_then(|resource_id| available_resources.get(resource_id))
                        .map(|(resource_id, capacity)| (shift_idx, place_idx, *resource_id, capacity.clone()))
                })
                .flat_map(move |(shift_idx, place_idx, resource_id, capacity)| {
                    vehicle.vehicle_ids.iter().filter_map(move |vehicle_id| {
                        let job_id = format!("{vehicle_id}_reload_{shift_idx}_{place_idx}");
                        let capacity = capacity_map(capacity.clone());
                        job_index.get(&job_id).map(|job| (job.clone(), (capacity, resource_id)))
                    })
                })
        })
        .collect()
}

#[allow(clippy::type_complexity)]
fn extract_feature_map(features: &[Vec<Feature>]) -> Result<(Vec<Vec<String>>, Vec<Vec<String>>), GenericError> {
    let global_objective_map: Vec<Vec<String>> = features
        .iter()
        .map(|features| features.iter().filter_map(|f| f.objective.as_ref().map(|_| f.name.clone())).collect())
        .collect();

    // NOTE: this is more performance optimization: we want to minimize the size of InsertionCost
    //       which has the same size as local_objective_map. So, we exclude some objectives which
    //       are not really needed to be present here.
    let exclusion_set = &["min_unassigned"].into_iter().collect::<HashSet<_>>();
    let local_objective_map: Vec<Vec<String>> = features
        .iter()
        .flat_map(|inner| {
            inner
                .iter()
                .filter_map(|f| f.objective.as_ref().map(|_| f.name.clone()))
                .filter(|name| !exclusion_set.contains(name.as_str()))
        })
        // NOTE: there is no mechanism to handle objectives on the same level yet, so simply move
        //       them to a separate level and rely on non-determenistic behavior of ResultSelector
        .map(|objective| vec![objective])
        .collect();

    // NOTE COST_DIMENSION variable in vrp-core is responsible for that
    if local_objective_map.len() > 6 {
        println!("WARN: the size of local objectives ({}) exceeds pre-allocated stack size", local_objective_map.len());
    }

    // TODO generate and return alternatives

    Ok((global_objective_map, local_objective_map))
}

fn get_threshold(options: &Option<BalanceOptions>) -> Option<f64> {
    options.as_ref().and_then(|o| o.threshold)
}

fn get_tour_order_fn() -> TourOrderFn {
    TourOrderFn::Left(Arc::new(|single| {
        single
            .as_ref()
            .map(|single| &single.dimens)
            .map(|dimens| {
                dimens.get_job_order().map(|order| OrderResult::Value(order as f64)).unwrap_or_else(|| {
                    dimens.get_job_type().map_or(OrderResult::Default, |v| {
                        match v.as_str() {
                            "break" | "reload" => OrderResult::Ignored,
                            // job without value
                            _ => OrderResult::Default,
                        }
                    })
                })
            })
            // departure and arrival
            .unwrap_or(OrderResult::Ignored)
    }))
}
