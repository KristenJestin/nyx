-- Revert v48: drop the persisted human-turn ledger. No index / FK of its own — a plain drop.
DROP TABLE IF EXISTS human_turns;
