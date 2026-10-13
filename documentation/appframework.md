# Liminal UI Framework & Runtime Specification

Bem-vindo à documentação oficial do **Liminal UI Framework**. Este documento descreve a arquitetura do framework, o formato de empacotamento de aplicativos (`.xapp`), a estrutura de desenvolvimento (AIML, CSS, Lua) e como a *Runtime* executa esses pacotes no topo do sistema operacional.

---

## 1. Visão Geral do Framework

O **Liminal** é um framework de interface gráfica e ecossistema de aplicativos projetado para o nosso sistema operacional Unix-like em Rust. Ele desacopla a lógica do aplicativo da interface visual através de uma arquitetura modular baseada em três pilares:

* **AML (Application Interface Markup Language):** Estrutura de marcação estilo XML para compor a árvore de elementos visuais.
* **CSS (Cascading Style Sheets):** Estilização flexível e desacoplada das caixas e componentes.
* **Lua:** Lógica leve e segura executada em *sandbox* para interagir com a UI.

---

## 2. Estrutura de um Projeto de Aplicativo

Antes de ser empacotado, o código-fonte de um aplicativo segue esta árvore de diretórios:

```text
meu-app/
├── AppManifest.xml
└── src/
    ├── main.aiml
    ├── main.css
    └── main.lua

```

### Exemplo dos Arquivos Fontes

* **`AppManifest.xml`**
```xml
<Application name="Bloco de Notas" version="1.0" entry="src/main.lua">
    <Window width="400" height="300" title="Meu Bloco de Notas"/>
</Application>

```


* **`src/main.aml`**
```xml
<aml>
  <head>
    <title>Window</title>
    <meta name="developer" content="com.imagine">
  </head>
  <body>
    <textarea id="editor" placeholder="Digite algo aqui..."/>
    <button id="btn-salvar" text="Salvar Arquivo"/>
  </body>
</aml>

```


* **`src/main.lua`**
```lua
local UI = require("imagine.liminal-ui-toolkit")

local btn = UI.get_element_by_id("btn-salvar")
local editor = UI.get_element_by_id("editor")

btn:on_click(function()
    print("Botão clicado! Texto atual: " .. editor:get_text())
end)

```



---

## 3. O Fluxo de Build e o Empacotamento (`.xapp`)

O desenvolvedor utiliza a ferramenta de linha de comando (`CLI`) do framework para compilar e preparar o pacote.

### Comando de Build

```bash
framework-cli build

```

### O que o CLI faz durante o Build:

1. **Varre** o diretório do projeto e valida o `AppManifest.xml`.
2. **Cruza** os dados do sistema operacional de destino, a versão do framework e do app.
3. **Gera** dinamicamente um arquivo `app.json` contendo metadados de execução, hash de integridade e permissões.
4. **Empacota** tudo em um único arquivo de distribuição final com extensão **`.xapp`** (eXtensible Application, parte da Imagine Application Foundation aka. `ApplicationFoundation`).

---

## 4. Arquitetura da Runtime (Execução no SO)

A **Liminal Runtime** é o motor que roda no espaço de usuário (*user-space*) do SO para dar vida aos aplicativos `.xapp`. Ela opera em quatro etapas principais:

```
[ Arquivo .xapp ] ---> [ 1. Descompactador & Leitor de app.json ]
                              |
                              v
                       [ 2. Parser AML + Motor CSS (Layout Box Model) ]
                              |
                              v
                       [ 3. Interpretador Lua (Sandbox de Lógica) ]
                              |
                              v
                       [ 4. Renderizador Gráfico (Framebuffer / Primitivas) ]

```

1. **Carregamento e Validação:** A runtime lê o `app.json` gerado no build para verificar permissões de segurança e compatibilidade de API.
2. **Construção da Árvore de Elementos (DOM/Scene Graph):** O arquivo `.aml` é interpretado em uma árvore de nós, enquanto as regras do `.css` calculam o modelo de caixa (*Box Model*, margens, padding, larguras).
3. **Ambiente de Execução Lua:** A máquina virtual Lua é inicializada e injeta as funções globais do toolkit (como `UI.get_element_by_id`). O script do desenvolvedor (`main.lua`) é executado em um ambiente isolado.
4. **Composição Gráfica:** As alterações de estado vindas do script Lua atualizam a árvore visual, que por sua vez redesenha as primitivas gráficas diretamente no framebuffer do sistema.

---

## 5. Próximos Passos do Desenvolvimento

* [ ] Implementar o parser básico de AIML em Rust.
* [ ] Integrar o motor de estilos baseados em um subconjunto de CSS.
* [ ] Configurar a máquina virtual Lua para interagir com os nós da árvore de UI.
* [ ] Desenvolver o comando CLI de build para gerar o pacote `.xapp`.

---

*Documentação mantida pelo Imagine OS Team.*