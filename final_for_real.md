Vea Validator

A document explaining in human terms, what this application is, how it works, etc.

The agent aiding in the production of this application must make sure every point applies, no exception.

Desired Wishlist:



Technical implementation:

- there are "bridge watchers", these are, programs that watch over a one-way bridge. Each one of them is the "master" of its bridge.

For example, imagine we had two bridges, Arb -> Mainnet, and Arb -> Gnosis. Then, we need two bridge watchers, one watcher for each bridge. Easy so far, right?

From this point onwards, assume I'm discussing the bridge watchers. Everything I mention here is spawned or handled by a bridge watcher:

Each responsibility is completely separate from the others (separation of concerns). Things that need to be done, per bridge:
- track the epochs, and trigger these effects (this doesn't rely on the blockchain, its time tracking):
  - BEFORE_EPOCH_BUFFER seconds before epoch ends (bef_epoch)
  - AFTER_EPOCH_BUFFER after epoch ends (aft_epoch)

bef_epoch -> check if there are any unsaved messages in the inbox, if so, call `saveSnapshot()`

aft_epoch -> check if last epoch was saved via saveSnapshot, if so, check if outbox has a claim pending (e.g. someone already submitted theirs). if not, make a claim.

on listen to Claim having been made event: verify the hash on the outbox is the same as the hash in the inbox for the given epoch. if not, trigger challenge routine with the correct hash

on listen to Start verification event: apparently, nothing?! research...

challenge routine: call outbox.challenge() (with the proper arguments), then you go to inbox.sendSnapshot()

on listen to SnapshotSent(ticketID) : this means that, from time that event was emitted, after x time, it will arrive to the other side (from arb -> eth) . this should take 7 days, so, wait 7 days and 10 minutes from the moment this is emitted, and then trigger L1 bridge handler: call constructOutboxProof(ticketID), and then Outbox.executeTransaction()
