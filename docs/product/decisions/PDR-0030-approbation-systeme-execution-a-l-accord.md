# PDR-0030 - Approbation du scope Système : l'accord humain EXÉCUTE l'effet, l'appel MCP attend sa réponse

**Status: validated**

Renverse le protocole d'approbation du scope Système livré en phase 4 de la Spec « Scope Système :
terminaux & commandes hors-projet (singleton) » (`01KY4FJXT0J2XSP53KK61ZA5R8`). Le protocole
claim/replay — refus porteur d'un jeton, décision humaine, rejeu par l'agent avec le jeton — est
remplacé par : **l'accord humain déclenche l'effet côté serveur**, et **l'appel MCP protégé bloque
jusqu'à la décision**, plafonné sous le timeout de requête des clients MCP. Décision de
l'utilisateur, prise à la revue humaine `01M1C7C8K0Q1WBHDQKZ29GH8YJ` (findings
`01M1C7CWMF4ABHRWT1DGBJTXW3` et `01M1C7DVQ53JJ90HGD5K1HDY11`, tous deux `critical`), en connaissance
de ce que l'échange coûte.

## Context

### Le défaut : un canal qui n'existe pas

Le protocole d'origine remettait à l'appelant, dans l'erreur `approval_required`, un bag
`error.data` complet : `request_id`, jeton opaque de 244 bits, `expires_at`, et l'appel de rejeu tout
prêt. Le test end-to-end validait ce bag sur le vrai fil JSON-RPC. Le gate CDP validait l'app réelle.
Les deux étaient verts.

Entre le fil et le modèle, il y a le CLIENT. **Aucun client MCP n'est tenu d'exposer `error.data` au
modèle**, et le plus répandu ne remonte que `error.message`. Un agent MCP réel, mis à l'épreuve du
protocole de bout en bout, a reçu deux fois un message qui lui *désignait* des données qu'il ne lui
donnait pas. Conséquence mesurée : `system_approval_status` exigeait `request_id` + `token`, le rejeu
exigeait `approval_request_id` + `approval_token`, les deux portes étaient closes, et **100 % des
opérations mutantes du scope Système étaient inatteignables quelle que soit la décision humaine**.
Le ledger, la migration, les trois codes de fil et la matrice de garde étaient morts à l'usage.

Le gate ne pouvait pas le voir : il pilotait le fil, jamais un vrai client. C'est la limite de la
règle d'or telle qu'elle était appliquée — *observer l'app réelle* ne suffit pas quand la capacité
s'adresse à un agent : il faut *observer un agent réel*.

### Le piège du correctif naïf

Inliner le ticket dans `message` contourne par le bas un garde-fou existant. `RpcError::describe()`
rend `"{code}: {message}"` **sans** le bag `data`, et son commentaire disait pourquoi : `data` peut
porter un jeton sur l'arm `approval_required`, et cette chaîne est écrite en base (le journal
d'issue du ledger). Mettre le secret dans `message` l'aurait journalisé en clair.

### Ce que la modale coûtait, aussi

Au même test manuel, la grande modale d'approbation s'est révélée juste sur le fond et fausse sur la
forme : elle dit clairement l'opération, la cible et le demandeur, mais elle prend tout l'écran et
STOPPE l'élan de l'utilisateur. Symétriquement, un agent à qui l'on remet un ticket et que l'on
renvoie casse son propre élan : créer une commande, c'est en général pour la lancer dans la foulée.

## Decision

### 1. L'accord humain EXÉCUTE l'effet

La décision déclenche l'opération **côté serveur**, sur la charge canonique que le ledger détient
déjà — celle-là même que la carte d'approbation affiche. L'humain approuve ce qu'il voit ; le serveur
exécute ce qu'il a montré. Il n'y a plus rien de secret à faire voyager jusqu'au modèle, donc le
finding critique disparaît **par construction** et non par un double rendu.

- `system_command_approvals` perd `token_hash` et gagne `tool_name` (l'outil à réexécuter, distinct
  d'`operation_kind` que la matrice peut qualifier — `manage_command_group.add`) et `result_json`
  (la réponse de l'effet, que lira l'agent revenu après coup). Migration
  `20260831160000_system_approvals_execute_on_grant`.
- `grant()` pose `state`, `decided_at` **et** `claimed_at` dans un seul `UPDATE … WHERE state =
  'pending' AND expires_at > now`. Le one-shot devient une propriété de la transition d'accord :
  un seul de N clics concurrents voit la ligne, donc l'effet part exactement une fois.
- L'exécuteur repasse par le **dispatcher d'outils ordinaire** (`McpRuntime::execute_granted`), avec
  la matrice suspendue pour cet appel (`system_approvals::with_granted_authority`, un thread-local
  que rien sur le fil ne peut atteindre). Une seule table de routage : l'opération que l'utilisateur
  approuve et celle que nyx exécute ne peuvent pas diverger.
- `system_approval_status` n'exige plus de jeton : c'est une lecture de décision et de résultat,
  jamais une autorisation.

### 2. L'appel MCP protégé ATTEND sa réponse

- L'appel **bloque** jusqu'à la décision et l'exécution de l'effet.
- **Plafonné à 45 s** (`APPROVAL_WAIT_CAP_MS`). Le plafond n'est pas une préférence : c'est le
  *timeout de requête du client MCP*, hors de notre contrôle. Les clients de référence sont à 60 s ;
  bloquer 60 s, c'est répondre à l'instant précis où le client abandonne, et l'agent reçoit une
  erreur de transport au lieu de notre réponse propre. 45 s laisse quinze secondes de marge.
- Décision dans la fenêtre ⇒ l'agent reçoit **le vrai résultat de l'opération**, en un aller-retour.
- Au-delà ⇒ un `pending` explicite et actionnable — `request_id`, `expires_at`, et la phrase qui dit
  quoi faire — **dans le RÉSULTAT de l'outil, pas dans une erreur**. C'est la leçon du défaut : un
  résultat d'outil est rendu au modèle par tous les clients, `error.data` par aucun de façon garantie.
  L'agent re-sonde avec `system_approval_status` : il attend quand même, en plusieurs sauts.
- Une DÉCISION reste une erreur (`invalid_state`) : « refusé » et « expiré » sont des refus de
  l'appel, et tout leur contenu tient dans `message`, que tout client rend.
- Le **TTL humain reste à 5 min**, et le compte à rebours affiché lit le même `expires_at` que le
  ledger applique : une seule horloge.

### 3. La modale devient un toast

L'approbation n'interrompt plus : toast riche, non bloquant, portant l'opération, la cible, le
demandeur, **l'environnement valeurs comprises** et le compte à rebours, avec *Approve once* et
*Deny* dedans. Le reste de l'app reste utilisable. La surface d'attention reste bornée, ordonnée par
date et dédupliquée par empreinte.

## Consequences

### Ce que l'on gagne

- La capacité est **atteignable**. Un agent dont le client n'expose que `error.message` mène une
  opération protégée de bout en bout — pinné par un test qui joue exactement ce client aveugle
  (`system_command_guard::tests::an_agent_that_can_only_see_error_messages_completes_a_protected_operation`).
- **Un aller-retour** dans le cas courant, contre trois. L'agent qui crée une commande la reçoit
  créée et peut la lancer.
- L'état mort « approbation accordée jamais réclamée » n'est plus constructible. Il l'était : l'index
  unique partiel ne couvre que `pending`, donc un rejeu créait une **seconde** demande pendant que la
  première dormait, accordée.
- Le garde-fou de `describe()` survit et change de motif : il n'y a plus de jeton, mais `data` reste
  un bag arbitraire et non borné, et cette chaîne va dans une colonne de journal de 200 caractères.

### Ce que l'on perd, assumé

- **La garantie « même appelant » disparaît avec le jeton.** N'importe quel client MCP local
  connaissant un `request_id` peut lire la décision et son résultat. Sur un serveur bindé sur la
  loopback, le modèle de menace « un autre client MCP local vole l'approbation » est mince, et ce
  qu'il obtiendrait est une lecture, jamais une autorisation : l'échange est jugé favorable.
- **L'effet part sans que l'agent soit présent.** Un humain qui approuve à la 90ᵉ seconde déclenche
  l'opération alors que l'appel de l'agent est déjà revenu. C'est voulu — l'utilisateur voit l'effet
  au moment où il l'autorise — mais cela signifie qu'une opération peut aboutir sans que personne
  n'en lise le résultat tout de suite. `result_json` est là pour ça.
- **Un travailleur MCP est immobilisé jusqu'à 45 s** par appel protégé en attente. Le pool en compte
  quatre ; trois restent libres. Aucune connexion SQLite n'est retenue pendant l'attente (chaque
  passe de sondage en prend une et la rend).
- La notification `system-approvals://changed` a dû **déménager** : émise au retour de l'appel, elle
  arrivait 45 s après que la décision fût nécessaire. Elle part maintenant du ledger lui-même, à
  l'instant où la ligne est écrite (`system_approvals::set_pending_notifier`, installé dans
  `Runtime::attach_events`).

### Ce que cela dit de la validation

La règle d'or (`docs/product/technical-architecture.md`) demande d'observer la capacité *fonctionner
dans l'app réelle*. Ce défaut montre son angle mort : une capacité dont le consommateur est un AGENT
n'est observée que par un agent réel, sur un vrai client, avec les limitations de rendu de ce client.
Piloter le fil JSON-RPC n'est pas piloter un client.
