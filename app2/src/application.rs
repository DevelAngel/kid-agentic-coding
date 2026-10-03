use std::marker::PhantomData;

use crate::{Defined, InitialWorkflow, Workflow};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ApplicationEntry<W> {
    marker: PhantomData<W>,
}

impl<W> ApplicationEntry<W>
where
    Workflow<W, Defined>: InitialWorkflow,
{
    pub fn workflow(self) -> Workflow<W, Defined> {
        Workflow::<W, Defined>::initial()
    }
}
