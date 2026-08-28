# Migrations — convention de nommage

Une migration se nomme **`AAAAMMJJhhmmss_description_en_snake_case`** : l'instant UTC de sa
création, sur 14 chiffres, puis un nom parlant.

```sh
# Le nom se génère, il ne se choisit pas :
date -u +%Y%m%d%H%M%S      # → 20260818020000
mkdir crates/nyx-core/migrations/20260818020000_create_app_settings
# puis up.sql et down.sql dans ce dossier.
```

## Pourquoi

Diesel identifie une migration **par le numéro de son dossier**, et par rien d'autre :
`__diesel_schema_migrations` ne stocke que ce numéro. Deux dossiers qui portent le même numéro
produisent une divergence silencieuse — le second est déclaré « déjà appliqué », son `up.sql` ne
tourne jamais, et `schema.rs` continue de déclarer les colonnes qu'il devait créer. Chaque lecture
de la table concernée échoue alors à l'exécution.

Ce n'est pas théorique. Le compteur l'a produit deux fois :

- v2.7 et `feature/v2.8-engine` ont toutes deux écrit un `34` ;
- cinq worktrees numérotant en parallèle depuis le même point ont fait désigner par
  `00000000000050` **trois** migrations différentes, sur une base **partagée** par les cinq.

L'horodatage supprime la course : deux worktrees ne peuvent collisionner que s'ils génèrent leur
migration dans la même seconde.

## Ce qui ne se fait pas

- **Ne jamais renommer une migration existante.** Le numéro est l'identité d'une migration
  *appliquée* : la renommer la rend pendante, et Diesel la **rejoue** sur toutes les bases en
  service.
- **Ne pas repartir au compteur.** L'ère du compteur s'arrête à `00000000000051`. La série garde
  ses trous (pas de `19`) — le nombre de dossiers n'est pas le plus grand numéro.
- **Ne pas oublier `down.sql`.** Les tests de migration rejouent la chaîne à l'envers
  (`revert_last_migration`).

## La garde

`crates/nyx-core/src/db.rs`, module de test `migration_naming_convention` : il refuse un dossier
au format compteur créé après `00000000000051`, deux dossiers partageant un numéro, et tout nom
qui n'est pas `<14 chiffres>_<description>`. Il vérifie aussi que l'héritage compteur est resté
intact **numéro par numéro** — la série exacte, trou compris — et rien de plus : la description qui
suit le numéro n'est comparée à rien, parce que Diesel ne la lit pas (voir « Pourquoi » ci-dessus).
Renuméroter un dossier existant échoue ici ; le redécrire ne change l'identité de rien.

`schema_health` (même fichier) ferme l'autre moitié du problème : au démarrage, il confronte le
registre au schéma réel et refuse de démarrer si une version inscrite a laissé son objet absent.
