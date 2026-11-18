# Itens novos

O Turbo tem que funcionar em qualquer Windows 10 22H2 ou Windows 11, não só no PC de quem escreveu o item. Um item que só acerta num idioma, num nome de adaptador ou numa placa deixa de ser aceito.

## Antes de gravar

1. Detecte se o item se aplica. Chave, serviço, placa ou monitor ausente vira estado **Não se aplica**, com `offer_apply = false`. Isso não é erro e não entra no anel de pontuação.
2. Falha de leitura (consulta inválida, acesso negado inesperado) continua **Não lido**, isolada dos outros itens.
3. Conta sem administrador: leitura que exige admin fica **Precisa de admin**. Aplicar sem elevação não muda nada.
4. Se o usuário já tem um valor igual ou melhor, o estado é **OK**. Otimizar não troca por uma opção pior.

## Como identificar

- Registro e WMI: `RegistryKeyword`, valor numérico e GUID. Comparação sem diferenciar maiúsculas e sem depender de `{}`.
- Não use o nome da conexão (`Ethernet`) nem o texto traduzido do Windows (`Ativado`, `Activado`, `Enabled`, `Desactivado`).
- Plano de energia: GUID base, personalidade e os índices de disco e CPU. Uma duplicata de Desempenho Máximo continua Desempenho Máximo.
- Várias placas entram juntas. Wi-Fi não recebe o ajuste da placa cabeada. GPU e CPU podem ser NVIDIA, AMD ou Intel.
- Notebook: plano que aumenta o consumo só entra com aviso de bateria. Não force esse plano em silêncio.

## Ao aplicar e ao reverter

- Cada passo e cada erro vão para `log.jsonl` e para `turbo.log`.
- Grave, releia e, se o valor não confirmou, restaure o original.
- Reverter apaga só o que o próprio item criou (por exemplo o plano duplicado pelo Turbo). Plano embutido do Windows não é apagado.

## Teste

Acrescente um caso em `src/engine.rs` ou `src/rules.rs` com `FakeBackend`. Cubra pelo menos: outro idioma, hardware diferente, item ausente e conta sem admin. `cargo test --lib` tem que passar sem Windows.
