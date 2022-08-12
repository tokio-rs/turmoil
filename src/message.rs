use std::any::Any;
use std::fmt;
use std::io::Write;

pub trait Message: fmt::Debug + Send + Any {
    /// Serialize the message to JSON.
    ///
    /// This feature is used when logging a Turmoil execution.
    fn write_json(&self, dst: &mut dyn Write);
}

pub(crate) fn downcast<T: Message>(message: Box<dyn Message>) -> T {
    use std::any::{type_name, TypeId};

    // Make sure the types match.
    assert_eq!(
        (&*message).type_id(),
        TypeId::of::<T>(),
        "unexpected message type; expected {}; got {:?}",
        type_name::<T>(),
        message
    );

    // YOLO
    unsafe {
        let ptr = Box::into_raw(message);
        *Box::from_raw(ptr as *mut _)
    }
}
