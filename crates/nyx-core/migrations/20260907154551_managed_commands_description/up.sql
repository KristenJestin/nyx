-- Migration 20260907154551: `managed_commands.description` — LA NOTE POUR L'AGENT.
--
-- Une commande managée dit CE QU'ELLE LANCE (`command`) et OÙ (`subfolder`), jamais QUAND ni
-- POURQUOI. L'agent qui reçoit le bloc ambiant au SessionStart lit `dev · bun run dev · idle` et
-- doit deviner si c'est le serveur à démarrer avant de toucher au front, la suite à rejouer avant
-- de conclure, ou un service partagé qu'il ne doit surtout pas redémarrer. Cette colonne est
-- l'endroit où l'humain écrit cette phrase-là, UNE fois, pour toutes les sessions.
--
-- Elle appartient au TEMPLATE et non à l'instance, exactement comme `env` (v44) : « quand lancer
-- cette commande » ne change pas d'un workspace à l'autre — c'est la nature de la commande, pas de
-- sa matérialisation. La pendre à l'instance la dupliquerait par workspace et la ferait diverger.
--
-- Nullable, sans index / FK / CHECK : l'immense majorité des commandes n'en a pas, une note
-- absente rend une ligne de bloc SANS la ligne de note, et aucune valeur — si longue ou si
-- malformée soit-elle — ne peut rendre une commande inexécutable. Le rendu la tronque, le stockage
-- ne la contraint pas. Ajoutée EN DERNIER sur `managed_commands`, après `group_id` / `group_order`
-- (décodage positionnel), même forme qu'`env`.

ALTER TABLE managed_commands ADD COLUMN description TEXT;
