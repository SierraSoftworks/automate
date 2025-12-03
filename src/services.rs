pub trait Services {
    fn kv(&self) -> impl crate::db::KeyValueStore;
    fn queue(&self) -> impl crate::db::Queue;
}