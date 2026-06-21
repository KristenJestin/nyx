# Vision produit

## État

**Status: validated direction, details proposed**

## Définition

nyx est un **gestionnaire de terminaux orienté travail agentique**. Il doit devenir l'environnement local dans lequel un travail logiciel est défini, exécuté, observé, reviewé et validé.

```text
Intention -> Exécution -> Résultat -> Review -> Décision
PRD          Agents       Git         Threads    Validation
             Terminaux
             Services
```

Le terminal reste une surface essentielle et immédiate. nyx ne doit pas devenir un gestionnaire de projet générique dont le terminal serait un onglet secondaire.

## Promesse

Depuis un projet nyx, l'utilisateur doit pouvoir :

1. définir ou sélectionner le travail à réaliser ;
2. lancer un ou plusieurs agents dans le bon contexte ;
3. voir les terminaux et services réellement utilisés ;
4. reprendre les sessions exactes ;
5. comprendre les fichiers et commits produits ;
6. reviewer de gros changements locaux ;
7. discuter avec l'agent sur des commentaires ancrés ;
8. décider humainement de la validation ;
9. conserver les décisions et le contexte sans dépendre du chat.

## Principes validés

- **Produit unique** : aucune frontière visible « nyx versus Depot ».
- **Local-first** : état, code, processus et reviews restent locaux par défaut.
- **Même vérité UI/MCP** : l'agent et l'utilisateur manipulent les mêmes entités.
- **Contrôle humain** : l'agent répond et propose ; l'utilisateur garde les décisions irréversibles, notamment la résolution des commentaires.
- **Générique** : aucune logique PalBank, Claude Code ou Depot câblée dans le domaine central quand un concept générique suffit.
- **Terminal immédiat** : l'orchestration ne doit jamais dégrader la frappe, le rendu ou la simplicité d'ouverture d'un shell.

## Directions proposées

- Workflow simple par défaut ; phases, prototypes et gates avancés sur activation.
- L'interface met en avant la prochaine action et l'attention requise, pas la totalité de la machine d'état.
- Le **workspace** relie intention et runtime ; aucune entité `Execution` (rejetée, PDR-0008).
- Les capacités avancées de Depot ne seront absorbées qu'après preuve d'usage.

## Non-objectifs

- Devenir un IDE généraliste ou un éditeur de code complet.
- Remplacer Git ou une forge distante pour la collaboration d'équipe.
- Construire un moteur terminal propriétaire.
- Afficher toute la richesse de Depot dans la navigation principale.
- Conserver deux backends métier ou deux autorités projects/workspaces.
- Maintenir Tauri en parallèle « au cas où » une évolution future de WebKitGTK résoudrait les problèmes observés.
- Laisser Electron main/preload devenir un second backend métier en TypeScript.

## Risque principal

Le risque n'est plus le renderer terminal. Le risque est de fabriquer une énorme plateforme interne avant d'avoir validé la boucle quotidienne :

```text
travail actif -> agent -> changements -> review -> corrections
```

Chaque version doit améliorer cette boucle avant d'ajouter des sous-systèmes avancés.
