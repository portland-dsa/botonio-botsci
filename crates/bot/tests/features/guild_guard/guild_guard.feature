Feature: The home-guild guard
  Botonio serves only the chapter's configured server - Sonic's server - and
  removes itself from any other server it finds itself in, so a stray invite can
  never make it act somewhere it does not belong.

  Background:
    Given Botonio's home server is Sonic's server

  Scenario: Botonio stays in its home server
    When Botonio receives a guild-create for Sonic's server
    Then Botonio does not leave any server

  Scenario: Botonio flees a server it is added to
    When Botonio receives a guild-create for Eggman's server
    Then Botonio leaves Eggman's server

  Scenario: Botonio flees an unauthorized server it was already in at startup
    When Botonio receives a startup guild-create for Eggman's server
    Then Botonio leaves Eggman's server

  Scenario: Botonio survives a refused exit
    Given Eggman's server will refuse Botonio's exit
    When Botonio receives a guild-create for Eggman's server
    Then Botonio attempts to leave Eggman's server
    And Botonio does not crash
