# nyx-v2

Espace de conception de la prochaine génération de nyx.

Le produit visé reste **nyx** : un gestionnaire de terminaux qui devient le cockpit local du travail agentique, de l'intention jusqu'à la review. Depot doit à terme être absorbé puis supprimé comme application distincte.

La source de vérité produit se trouve dans [`docs/product`](docs/product/README.md).

La plateforme cible est désormais actée : **Electron comme shell desktop unique, React comme renderer embarqué et un exécutable sidecar Rust comme backend métier unique**. Le renderer passe par l'IPC Electron sécurisé ; Electron supervise le sidecar via des pipes locaux. Le repository reste un monorepo léger et Tauri n'est plus maintenu comme plateforme active.
