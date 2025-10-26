use std::{any::Any, collections::HashMap};

#[derive(Debug)]
pub struct StateBox {
    store: HashMap<String, Box<dyn Any>>,
}

impl StateBox {
    pub fn new() -> Self {
        StateBox {
            store: HashMap::new(),
        }
    }
    /// Inserts a key-value pair into the statebox.
    ///
    /// If the statebox already had this key, [`Err`] is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use statebox::StateBox;
    ///
    /// let mut states = StateBox::new();
    /// assert!(states.is_empty());
    /// assert!(states.insert("Name", String::from("DitherDude")).is_ok());
    /// assert!(states.insert("Age", 37u8).is_ok());
    /// assert!(states.insert("Age", 44u8).is_err());
    /// ```
    pub fn insert<T: 'static>(&mut self, key: &str, value: T) -> Result<(), String> {
        if self.store.contains_key(key) {
            return Err(String::from(
                "Key already exists! If you wish to update this value, use `set()` method instead.",
            ));
        }
        self.store.insert(key.to_string(), Box::new(value));
        Ok(())
    }
    /// Removes a key-value pair from the statebox.
    ///
    /// If the statebox does not contain this key, [`Err`] is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use statebox::StateBox;
    ///
    /// let mut states = StateBox::new();
    /// states.insert("Name", String::from("DitherDude")).unwrap();
    /// assert!(states.remove("Age").is_err());
    /// assert!(states.remove("Name").is_ok());
    /// assert!(states.remove("Name").is_err());
    /// ```
    pub fn remove(&mut self, key: &str) -> Result<(), String> {
        if self.store.remove_entry(key).is_some() {
            Ok(())
        } else {
            Err(String::from("Cannot remove nonexistent key!"))
        }
    }
    /// Gets **a reference of** the value from the statebox with the specified key.
    ///
    /// If the statebox does not contain this key, [`None`] is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use statebox::StateBox;
    ///
    /// let mut states = StateBox::new();
    /// states.insert("Name", String::from("DitherDude")).unwrap();
    /// assert_eq!(states.get::<String>("Name"), Some(&String::from("DitherDude")));
    /// assert!(states.get::<u8>("Age").is_none());
    /// ```
    pub fn get<T: 'static>(&self, key: &str) -> Option<&T> {
        self.store.get(key)?.downcast_ref::<T>()
    }
    /// Replaces the value with the specified key. Note that this value does not need
    /// to be of the same type as the original value.
    ///
    /// If the statebox does not contain this key, [`Err`] is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use statebox::StateBox;
    ///
    /// let mut states = StateBox::new();
    /// // `states` does not contain "Name".
    /// assert!(states.set("Name", 24u8).is_err());
    /// assert!(states.get::<u8>("Name").is_none());
    /// states.insert("Name", 24u8).unwrap();
    /// // `states` contains "Name" as `24u8`.
    /// assert!(states.get::<usize>("Name").is_none());
    /// // "Name" is of type `u8` and not `usize`, so `None` was returned.
    /// assert_eq!(states.get::<u8>("Name"), Some(&24));
    /// assert!(states.set("Name", String::from("DitherDude")).is_ok());
    /// // `states` contains "Name" as `String::from("DitherDude")`.
    /// assert!(states.get::<u8>("Name").is_none());
    /// assert_eq!(states.get::<String>("Name"), Some(&String::from("DitherDude")));
    /// ```
    pub fn set<T: 'static>(&mut self, key: &str, value: T) -> Result<(), String> {
        if let Some(state) = self.store.get_mut(key) {
            *state = Box::new(value);
            Ok(())
        } else {
            Err(String::from(
                "Key not found. If you wish to create this value, use `insert()` method instead.",
            ))
        }
    }
    pub fn push<T: 'static>(&mut self, _key: &str, _value: T) -> ! {
        //Learned the '!' (bang) return type from Rust Kernel dev ;P
        // TODO: implement this properly, it's not really needed right now
        unimplemented!()
    }
    /// Removes and returns the value associated with the specified key.
    ///
    /// If the statebox does not contain this key, [`None`] is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use statebox::StateBox;
    ///
    /// let mut states = StateBox::new();
    /// states.insert("Name", String::from("DitherDude"));
    /// // `states` now contains "Name".
    /// assert_eq!(states.get::<String>("Name"), Some(&String::from("DitherDude")));
    /// // `states` still contains "Name".
    /// assert_eq!(states.pop::<String>("Name"), Some(String::from("DitherDude")));
    ///  // `states` does not contain "Name".
    /// assert!(states.get::<String>("Name").is_none());
    /// ```
    pub fn pop<T: 'static>(&mut self, key: &str) -> Option<T> {
        self.store
            .remove(key)?
            .downcast::<T>()
            .map(|x| Some(*x))
            .ok()?
    }
    /// Force inserts the key-value pair into the statebox.
    ///
    /// If the statebox **does not** contain this key, a new key-value pair is inserted.
    /// If the statebox **does** contain this key, its value is overwritten.
    ///
    /// # Examples
    ///
    /// ```
    /// use statebox::StateBox;
    ///
    /// let mut states = StateBox::new();
    /// assert!(states.get::<String>("Name").is_none());
    /// // `states` does not contain "Name".
    /// states.shove("Name", String::from("DitherDude"));
    /// // `states` contains key "Name" with value `String::from("DitherDude")`.
    /// assert_eq!(states.get::<String>("Name"), Some(&String::from("DitherDude")));
    /// states.shove("Name", 43u8);
    /// // `states` contains key "Name" with value `43u8`.
    /// assert!(states.get::<String>("Name").is_none());
    /// assert_eq!(states.get::<u8>("Name"), Some(&43));
    /// ```
    pub fn shove<T: 'static>(&mut self, key: &str, value: T) {
        if let Some(state) = self.store.get_mut(key) {
            *state = Box::new(value)
        } else {
            self.store.insert(key.to_string(), Box::new(value));
        }
    }
    /// Force removes the specified key from the statebox.
    ///
    /// If the statebox **does not** contain this key, nothing happens.
    /// If the statebox **does** contain this key, it is removed.
    ///
    /// # Examples
    ///
    /// ```
    /// use statebox::StateBox;
    ///
    /// let mut states = StateBox::new();
    /// assert!(states.get::<String>("Name").is_none());
    /// assert_eq!(states.yank("Name"), ());
    /// // `states` does not contain "Name", so nothing happened.
    /// states.insert("Name", String::from("DitherDude")).unwrap();
    /// assert_eq!(states.get::<String>("Name"), Some(&String::from("DitherDude")));
    /// assert_eq!(states.yank("Name"), ());
    /// // "Name" was removed from `states`. Note how output is the same as the prior `yank()`.
    /// assert!(states.get::<String>("Name").is_none());
    /// ```
    pub fn yank(&mut self, key: &str) {
        // WARNING: This function has VERY different connotation to the 'yank' from NVIM!
        self.store.remove(key);
    }
    pub fn len(&self) -> usize {
        self.store.len()
    }
    // This is to make Clippy happy
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }
}

impl Default for StateBox {
    fn default() -> Self {
        Self::new()
    }
}
