#![allow(async_fn_in_trait)]

use std::{future::Future, pin::Pin};

use crate::Sim;

pub trait Workload {
    fn name(&self) -> &str;

    async fn setup(&mut self);
    async fn run(&mut self);
    async fn verify(&mut self);
}

pub trait WorkloadFactory {
    fn build(&self) -> Box<dyn WorkloadObj>;
}

#[derive(Default)]
pub struct CompoundWorkload {
    workloads: Vec<Box<dyn WorkloadFactory>>,
}

impl CompoundWorkload {
    pub fn new() -> Self {
        Self {
            workloads: Vec::new(),
        }
    }

    pub fn add_workload<W>(&mut self, workload: W)
    where
        W: WorkloadFactory + 'static,
    {
        self.workloads.push(Box::new(workload));
    }

    pub fn apply(&self, sim: &mut Sim<'_>) {
        for workload in &self.workloads {
            let mut wk = workload.build();

            sim.client(wk.name().to_string(), async move {
                wk.setup().await;
                wk.run().await;
                wk.verify().await;

                Ok(())
            })
        }
    }
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

pub trait WorkloadObj {
    fn name(&self) -> &str;

    fn setup(&mut self) -> BoxFuture<'_, ()>;
    fn run(&mut self) -> BoxFuture<'_, ()>;
    fn verify(&mut self) -> BoxFuture<'_, ()>;
}

impl<T> WorkloadObj for T
where
    T: Workload,
{
    fn name(&self) -> &str {
        Workload::name(self)
    }

    fn setup(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(Workload::setup(self))
    }

    fn run(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(Workload::run(self))
    }

    fn verify(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(Workload::verify(self))
    }
}

/// A macro for defining workload structures for Turmoil simulations.
///
/// This macro simplifies the creation of workloads that can be used to test distributed systems.
/// It generates the necessary implementations for the `Workload` and `WorkloadFactory` traits,
/// allowing you to define reusable testing components.
///
/// # Parameters
///
/// * A struct definition with fields. Fields can be marked with `#[config]` to indicate they
///   are configuration parameters that should be provided when creating the workload.
///
/// * An implementation of the `Workload` trait with three methods:
///   - `setup`: Called before the workload runs to initialize any state
///   - `run`: Contains the main logic of the workload, with access to the simulation
///   - `verify`: Called after the workload completes to assert expected outcomes
///
/// # Examples
///
/// ```
/// use turmoil::{workload, Sim, workload::Workload};
/// use std::cell::RefCell;
/// use std::rc::Rc;
///
/// workload! {
///     struct TestWorkload {
///         #[config]
///         clients: usize,
///         #[config]
///         requests: usize,
///
///         errors: Rc<RefCell<usize>>,
///     }
///
///     impl Workload for TestWorkload {
///         async fn setup(&mut self) {
///             // Initialize state if needed
///         }
///
///         async fn run(&mut self, sim: &mut Sim<'_>) {
///             // Create clients and run test logic
///             let clients = self.clients;
///             let errors = self.errors.clone();
///
///             for i in 0..clients {
///                 sim.client(format!("client{}", i), async move {
///                     // Client logic
///                     Ok(())
///                 });
///             }
///         }
///
///         async fn verify(&mut self) {
///             // Verify results
///             let errors = self.errors.borrow();
///             assert_eq!(*errors, 0, "No errors should have occurred");
///         }
///     }
/// }
/// ```
#[macro_export]
macro_rules! workload {
    (
        struct $name:ident {
            $(
                #[config]
                $config_field_name:ident: $config_field_type:ty,
            )*

            $(
                $field_name:ident: $field_type:ty,
            )*
        }

        impl Workload for $impl_name:ident {
            fn name(& $name_self:ident) -> &str $name_block:block
            async fn setup(&mut $setup_self:ident) $setup_block:block
            async fn run(&mut $run_self:ident) $run_block:block
            async fn verify(&mut $verify_self:ident) $verify_block:block
        }
    ) => {
        #[derive(Clone)]
        pub struct $name {
            $(
                $config_field_name: $config_field_type,
            )*
        }

        const _: () = {
            struct Inner  {
                $(
                    $config_field_name: $config_field_type,
                )*
                $(
                    $field_name: $field_type,
                )*
            }

            impl turmoil::workload::Workload for Inner {
                fn name(&$name_self) -> &str $name_block
                async fn setup(&mut $setup_self) $setup_block
                async fn run(&mut $run_self) $run_block
                async fn verify(&mut $verify_self) $verify_block
            }

            impl turmoil::workload::WorkloadFactory for $name {
                fn build(&self) -> Box<dyn $crate::workload::WorkloadObj> {
                    Box::new(Inner {
                        $(
                            $config_field_name: self.$config_field_name.clone(),
                        )*
                        $(
                            $field_name: Default::default(),
                        )*

                    })
                }
            }
        };
    };
}
