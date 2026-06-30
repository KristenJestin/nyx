# PDR-0021 - Human task : surfaçage dans l'onglet Tasks ; Attention Inbox non requise

**Status: validated**

Précise PDR-0014 (le `executor = human`) côté surfaçage UI.

## Context

Une **human task** (task `executor = human`, PDR-0014) doit être vue par l'humain pour qu'il l'exécute (ex. « va créer la clé API dans le dashboard Stripe »). Question : faut-il l'**Attention Inbox** (Q-007, non conçue) pour la surfacer ?

## Decision

- **Surfaçage dans l'onglet Tasks de la spec.** Une human task apparaît dans la décomposition (onglet Tasks) avec un **traitement visuel distinct** : un badge **« toi »** au lieu de l'icône d'agent, mise en avant si `open` et **bloquante**.
- **Exception lecture-seule.** Les tasks **agent** sont en lecture pour l'humain (décompo gérée par l'agent). La **human task est le seul type sur lequel l'humain agit** : il la marque `done` (ou l'acquitte). Faire son étape ≠ gérer la décompo → cohérent avec PDR-0014.
- **Notification + traçage + blocage.** L'agent **annonce aussi la human task dans le chat** (notification immédiate) ; l'onglet Tasks la **trace** (open → done) ; une task agent **bloquée** affiche « bloquée par [human task] » et reprend quand l'humain passe la human task `done` (`task_dependencies`).
- **L'Attention Inbox n'est PAS un prérequis.** Les human tasks fonctionnent via l'onglet Tasks. L'**Attention Inbox** reste un **roll-up transverse différé** (Q-007) : agrégation cross-spec/projet de **tous** les signaux (human tasks + reviews prêtes + erreurs + agents en attente…), utile à l'échelle mais non nécessaire pour livrer les human tasks.

## Consequences

- IA : l'onglet Tasks décrit le surfaçage human task ; les mentions « surfacée dans l'Attention Inbox » deviennent « affichée dans l'onglet Tasks ».
- Q-007 reste ouverte mais **explicitement non bloquante** pour les human tasks et le signal de review.
- PDR-0014 inchangé sur le modèle ; ceci précise le surfaçage.
