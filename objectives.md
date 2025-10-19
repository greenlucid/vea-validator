alright. context, i've asleep and thought about a "big refactoring" in what the "objectives" of the application are.

writing here.

Features:

Validator is really a bunch of different actors with different responsibilities. No one does anything that should be someone's else responsibility.

Roles:

Epoch watcher, Notifies parts of the program whenever an epoch is "about to end", or "has ended"

Event listener, listens for events and notifies parts that things need to be done


Snapshot saver is notified whenever 

Claimer is notified whenever an epoch has ended, it makes a claim.

 checks if there's at least 1 unbridged message.
