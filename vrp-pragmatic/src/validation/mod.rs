//! This module provides functionality to validate problem definition for logical correctness.

use crate::json::problem::*;

pub struct ValidationContext<'a> {
    pub problem: &'a Problem,
    pub matrices: Option<&'a Vec<Matrix>>,
}

mod common;
use self::common::check_time_windows;

mod jobs;
use self::jobs::validate_jobs;

mod vehicles;
use self::vehicles::validate_vehicles;

impl<'a> ValidationContext<'a> {
    /// Creates an instance of `ValidationContext`.
    pub fn new(problem: &'a Problem, matrices: Option<&'a Vec<Matrix>>) -> Self {
        Self { problem, matrices }
    }

    /// Validates problem on set of rules.
    pub fn validate(&self) -> Result<(), String> {
        let errors = validate_jobs(&self)
            .err()
            .into_iter()
            .chain(validate_vehicles(&self).err().into_iter())
            .flatten()
            .collect::<Vec<_>>();

        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!("Problem has the following validation errors:\n{}", errors.join("\n")))
        }
    }

    /// Get list of jobs from the problem.
    fn jobs(&self) -> impl Iterator<Item = &Job> {
        self.problem.plan.jobs.iter()
    }

    /// Get list of vehicles from the problem.
    fn vehicles(&self) -> impl Iterator<Item = &VehicleType> {
        self.problem.fleet.vehicles.iter()
    }
}
